# User-extensible color themes and icon themes

Design spike · 2026-10-20 · **Proposed contract, not implemented behavior.**

Goal: a person can create, edit, share, select and live-reload Hutan's complete visual identity without rebuilding Hutan or running extension code. This document is implementer-brief input; an accepted ADR should ratify the persistent format before TH-1. Repository observations below are static source inspection, not runtime verification. No builds, tests, application launches, user configuration writes or paid calls were performed for this spike. Only this document was written.

## Recommendation in one paragraph

Use separate **data-only TOML color packs and SVG icon packs**, selected independently. A color pack covers chrome, terminal, state/diff roles, typography, density and effects; an icon pack maps stable semantic ids to monochrome SVGs, independent of a particular vendor's filenames. Ship `hutan-dark`, `hutan-light` and `phosphor` as embedded packs parsed by the same loader as user packs. Custom packs explicitly extend an immutable built-in and may override just one token/icon. Put packs in the existing Hutan data directory, preserve last-good snapshots on bad edits, and surface problems through Settings' existing diagnostics. No extension host, network access, daemon theme registry, CSS, Lua, JavaScript, downloaded fonts, marketplace or project auto-loading is needed.

## 1. Inventory: what exists now

All paths in this section are relative to the Hutan checkout unless marked Orca. Line numbers describe the inspected sources and may drift as concurrent work lands.

### 1.1 Chrome palette: complete `Theme` inventory

`crates/desktop/src/theme.rs:102-134` has a `dark` boolean and these **16 color fields**. Palette values are at `theme.rs:136-155,183-207`; the dark border is really translucent white, **not** the opaque `DARK_PALETTE[4]` value `#3D3C39` (that table slot is bypassed).

| Current field | Light | Dark | Proposed canonical id |
|---|---|---|---|
| `bg` | `#FAF9F5` | `#262624` | `ui.background` |
| `sidebar` | `#F3F1EA` | `#1F1E1D` | `ui.sidebar.background` |
| `sidebar_accent` | `#E9E6DC` | `#2E2D2B` | `ui.sidebar.selected` |
| `right_sidebar` | `#F8F7F2` | `#1A1918` | `ui.sidebar-right.background` |
| `right_sidebar_accent` | `#F0EEE6` | `#2A2927` | `ui.sidebar-right.selected` |
| `surface` | `#FFFFFF` | `#30302E` | `ui.surface` |
| `surface_hover` | `#F0EEE6` | `#3A3936` | `ui.surface.hover` |
| `border` | `#E6E3D9` | white, alpha 0.07 | `ui.border` |
| `fg` | `#1F1E1D` | `#F5F4EF` | `ui.foreground` |
| `muted` | `#6F6D66` | `#A6A49C` | `ui.text.muted` |
| `faint` | `#A8A59C` | `#6F6D66` | `ui.text.faint` |
| `accent` | `#E0692E` | `#EA7A43` | `ui.accent` |
| `accent_soft` | `#FBE9DF` | `#3E2C23` | `ui.accent.soft` |
| `success` | `#3F8F5A` | `#6BBF85` | `state.success` |
| `warning` | `#C98A12` | `#E2AE45` | `state.warning` |
| `danger` | `#C8452F` | `#E87562` | `state.error` |

Additional colors/effects are methods, not fields (`theme.rs:220-245`): `on_accent` is always white; shadow is black alpha 0.12 light / 0.40 dark, offset (0,8), blur 24, spread 0; scrim is black alpha 0.28 / 0.50; drop-zone is accent alpha 0.22 / 0.28. These must not remain hidden hard-coded palette choices after cutover. Left-nav hover currently uses `fg` alpha 0.08 (`workbench/sidebars.rs:183-186`). Proposed ids: `ui.on-accent`, `ui.shadow`, `ui.scrim`, `ui.drop-target`, `ui.sidebar.hover` plus numeric shadow geometry in §3.4.

### 1.2 Terminal: complete palette and rendering rules

`crates/settings/src/themes.rs:10-13,78-88,100-125` defines only two built-ins, selector `auto`, and a `TerminalTheme { name, foreground, background, cursor, selection, ansi: [Rgb;16] }`. `Rgb` accepts exactly `#rrggbb` (`themes.rs:15-68`). Here is every current terminal palette value, in index order:

| Role/index | `hutan-dark` | `hutan-light` |
|---|---|---|
| foreground | `#EAE8E1` | `#1F1E1D` |
| background | `#262624` | `#FAF9F5` |
| cursor | `#EA7A43` | `#E0692E` |
| selection background | `#4B3B30` | `#F3D5C3` |
| 0 black | `#4A4944` | `#1F1E1D` |
| 1 red | `#E87562` | `#B93A26` |
| 2 green | `#6BBF85` | `#2F7A48` |
| 3 yellow | `#E2AE45` | `#8A6300` |
| 4 blue | `#72A5EC` | `#2B62B8` |
| 5 magenta | `#C996D8` | `#8E4EA8` |
| 6 cyan | `#5FC2C6` | `#167780` |
| 7 white | `#D6D4CC` | `#6F6D66` |
| 8 bright black | `#7A7870` | `#57554F` |
| 9 bright red | `#F08C7A` | `#C8452F` |
| 10 bright green | `#86D19C` | `#3F8F5A` |
| 11 bright yellow | `#EEC261` | `#9A6F00` |
| 12 bright blue | `#8FB9F2` | `#3D77D1` |
| 13 bright magenta | `#DAAEE6` | `#A35FBF` |
| 14 bright cyan | `#7DD3D6` | `#23909A` |
| 15 bright white | `#F5F4EF` | `#8C8A82` |

The same palettes are duplicated as `TerminalPalette::HUTAN_DARK/HUTAN_LIGHT` in `crates/terminal_view/src/theme.rs:33-102`. Remove this second source of palette truth during TH-1. The renderer takes the base plus application OSC overrides: indexed colors, foreground, background and cursor (`terminal_view/src/theme.rs:123-167`). Indices 16–231 use the standard six-level RGB cube; 232–255 use grayscale; explicit truecolor bypasses the palette (`theme.rs:199-223`). Inverse swaps fg/bg; hidden makes fg=bg; dim mixes fg 40% toward bg (`theme.rs:225-245`). Preserve these VT semantics, not themeable settings masquerading as palette tokens.

There is **no configurable selection foreground or cursor-text color today**: selection replaces the cell background (`terminal_view/src/element.rs:325-327`); the block cursor redraws its glyph in the terminal background (`element.rs:537-558`). TH-1 adds optional `terminal.selection.foreground` (default preserve the cell foreground) and `terminal.cursor.foreground` (default terminal background) to enable accurate imports; no change to OSC wire semantics is required.

Block overlays get success/error/neutral/accent from chrome (`desktop/src/theme.rs:248-273`), but standalone `BlockSettings::default` still has unrelated system hues `#30D158`, `#FF453A`, `#AAAAAF`, `#0A84FF` (`terminal_view/src/settings.rs:48-79`). Remove these competing palette literals. Renderer-derived block separator mixes bg toward fg 16%, highlight uses accent 13%, hover uses neutral 7%, and a folded-region fill mixes bg toward fg 7% (`terminal_view/src/element.rs:267-268,396-433`). Expose those *visual fills* as resolved overlay roles, while keeping dim/inverse/truecolor algorithms intact.

### 1.3 State, diff and syntax

- `Tone::{Warning,Success,Accent,Muted,Error}` maps directly to `Theme::{warning,success,accent,muted,danger}` (`desktop/src/sessions/attention.rs:26-46`; `workbench/panels/common.rs:26-33`). Working/starting→accent; idle→muted; blocked/approval/question→warning; unread completion→success; failed→error (`sessions/attention.rs:134-215`). `needs_you` is semantic state, **not a color**; native badge classification intentionally excludes successful finished-unread sessions (`attention.rs:49-61,96-116`). Packs cannot change classification, pulse behavior or approval meaning.
- Agent-tree glyphs: completed `✓`, failed `✗`, interrupted `⊘`, disconnected `⏹`, working `●`, blocked `▲`, idle `○`, unknown `?` (`sessions/agents.rs:59-67`). Task glyphs similarly use `○ ◎ ▶ ◆ ✓ ✗ ⊘` and the existing tone set (`sessions/tasks.rs:167-175`). Keep text/tooltips and non-color differentiation with icon replacement.
- File status uses added→success, modified→accent, deleted→danger, renamed→warning (`workbench/changes.rs:699-705`). Diff counts use success/danger; rows use fg text over success/error alpha 0.16, hunk uses accent alpha 0.10 and muted text (`workbench/chat/cards.rs:908-965`). Externalize `diff.added`, `diff.removed`, `diff.modified`, `diff.renamed`, `diff.added.background`, `diff.removed.background`, `diff.hunk.background`, `diff.hunk.foreground`.
- There is no shipped editor crate in the inspected crate inventory. The current chat code-block renderer shows the language label, then plain mono text in `theme.fg`, without token-level syntax colors (`workbench/chat/markdown_view.rs:206-258`). Diff is likewise line-level, not language highlighting (`cards.rs:931-964`). Markdown links use accent; inline code uses surface-hover (`markdown_view.rs:153-169`). **Do not claim syntax-theme support today.** Reserve the `syntax.*` namespace for the M11 editor/highlighter contract; imported syntax rules must produce an explicit unsupported-fields report until that consumer exists, not silently accepted dead configuration. `docs/design.md:199` describes syntax as M11 work.

### 1.4 Typography, density and additional geometry

Current `Tokens` fields are all listed at `desktop/src/theme.rs:10-38`; values are at `theme.rs:59-79`. Existing settings density table (`settings/src/appearance.rs:43-76`) and desktop additions disagree on status-bar height: settings says 22/26, desktop deliberately uses 24 for both. Preserve the rendered 24/24 in packs and eliminate the dead disagreement.

| Current token | Compact | Comfortable |
|---|---:|---:|
| sidebar_row | 26 | 30 |
| tab_height | 30 | 34 |
| pane_header | 30 | 36 |
| status_bar (rendered) | 24 | 24 |
| spacing | 4 | 6 |
| radius | 6 | 8 |
| card_radius | 10 | 12 |
| font_size | 12 | 13 |
| secondary_font_size | 11 | 12 |
| section_font_size | 11 | 11 |
| prose_font_size | 15 | 15 |
| prose_line_height | 1.6 | 1.6 |
| control_height | 28 | 28 |
| terminal_padding | 8 | 12 |
| block_gutter | 3 | 4 |

Other constants: status bar 24, title bar/right strip 36, pill radius 999, reading width 720, mascot size 96 (`theme.rs:40-49`). Nav is currently an exception to density: label 13, icon 16, line 18; strip button 28, gap 4 (`workbench/sidebars.rs:25-32`). Prose font is New York loaded from the macOS font files, then Georgia, then UI font (`workbench/ui.rs:16-59`). Terminal defaults are Menlo 13, line-height 1.2, cursor block/blink; font size clamps to 4–72 and line height to 0.8–3 (`desktop/src/settings.rs:111-145,173-195`). Native system UI sans is resolved through `.SystemUIFont` (`workbench/ui.rs:43`). Font selection, text sizing and density should be externalizable **visual defaults**, not permission to load arbitrary font binaries or override explicit user font/zoom preferences.

### 1.5 Selection and reload today

- Chrome selection is `appearance.appearance = system|light|dark`, with old `chrome` accepted as an alias; density defaults compact (`settings/src/appearance.rs:17-40`). No named UI-chrome theme is currently selectable.
- Terminal theme is a free string setting; `auto` follows the resolved chrome's polarity, not necessarily macOS when chrome is pinned; exact built-in name selects it; unknown name falls back to the matching built-in (`settings/src/themes.rs:127-153`; `desktop/src/theme.rs:210-217`; `desktop/src/settings.rs:88-89,166-188`). The generated Settings picker narrows choices to auto plus built-ins (`workbench/settings_schema.rs:217-220`), but hand-written TOML can contain other names.
- The settings watcher is `notify`, watches directories non-recursively so atomic replacements work, bounded queue 64, quiet debounce 100 ms, maximum delay 500 ms (`settings/src/watch.rs:1-24,72-81,105-150`). It watches settings/keymap sources, **not external theme/icon packs**; none are loaded now.
- Window appearance notification triggers re-render and resolution (`workbench/view.rs:696,1118-1122`). Settings changes rebuild theme, publish style, refresh all terminal views and report default fg/bg/cursor to sessions (`workbench/terminals.rs:1069-1115`). Terminal adapter forwards defaults to the backend (`sessions/terminal.rs:91-94,437-438`). Preserve this existing protocol path so OSC 10/11/12 queries remain accurate.
- `UiStyle` is currently an app-global copy of theme+density with dark/compact fallback (`workbench/style.rs:1-22`). New preview must not accidentally recolor another window through that global. Selection remains app-wide in this proposal; scoped preview style belongs to the previewing workbench/subtree.
- Data-dir resolution already honors nonempty `HUTAN_HOME`, otherwise `~/Library/Application Support/Hutan` (`settings/src/store.rs:24-65`). Settings errors already carry severity/file/dotted-key/message (`settings/src/error.rs:24-65`), and the sheet has a Problems footer (`workbench/settings_sheet.rs:1-12,117-124`). Reuse these conventions.

### 1.6 Icons and non-SVG substitutes

`crates/desktop/src/icons.rs:9-59` maps enum variants to **19 Lucide SVGs** with `include_bytes!`, exposes paths `icons/<file>.svg`, and serves embedded bytes using GPUI `AssetSource` (`icons.rs:79-106`). Current names: `search`, `list-checks`, `layout-dashboard`, `sparkles`, `history`, `git-compare`, `info`, `git-branch`, `gauge`, `rows-3`, `list-filter`, `refresh-cw`, `settings`, `chevron-down`, `chevron-up`, `chevron-left`, `ellipsis`, `plus`, `arrow-up-down`. Files use currentColor; e.g. `assets/icons/search.svg:1-4` is 24×24, stroke 2, rounded caps/joins. These are vendor names, not a complete semantic API. `Icon::for_panel` handles only eight panels (`icons.rs:61-76`); unhandled panels use a text glyph.

Residual decorative glyph categories to migrate, not merely the 19 embedded files:

| Surface | Current implementation/evidence |
|---|---|
| Panel definitions | Projects `≣`, History `⟲`, Changes `±`, Requests `ⓘ`, Overview `⊞`, Agents `⑂`, Resources `◫`, Tasks `☑`, Mawas `✦`, Memory `◈`, Notifications `◔`: `workbench/panels/mod.rs:125-226`; registry still types icon as `SharedString` (`registry.rs:33-35`) |
| Nav fallback | `⌕ ☑ ⊞ ✦`, optional icon plus glyph fields: `sidebars.rs:49-55,94-130` |
| Session type | Terminal `❯`, harness `◆`, direct `✦`: `panels/common.rs:192-196` |
| Agent and task states | `sessions/agents.rs:59-67`; `sessions/tasks.rs:167-175`, listed above |
| Tree folds, start, close, needs-you | `panels/sessions.rs:1067,1477,1489,1521,1658`; monitor folds `panels/agents.rs:467`; drawer close `agents_drawer.rs:395` |
| History provider/source and branch | Hutan `❯`, Claude `✳`, Codex `◈`, OMP `π`: `panels/history_model.rs:178-182`; folds/branch/checks `history_view.rs:349,1054,1380,1496` |
| Resources/Changes | fold/sort `▸▾▴`, more `⋯`, close `×`: `panels/resources.rs:652,705,787,841,987`; dirty `●`/diff `±`: `panels/changes.rs:219`; review folds `changes/tray.rs:942-979` |
| Chat | attach `＋` (`chat/attach.rs:683`), close `×` (`attach.rs:811`), folds `▸▾`, tool `✎ ⌕ ⚙` (`chat/cards.rs:313-366`), approval `●○` (`cards.rs:864-865`), context close (`rich_view.rs:278`) |
| Tabs, menus, empty panes | close `×`, start `▶`, check `✓`, layout `▾`: `render.rs:961,1815,1863,2136` |
| Recovery/status | `↻ ⚠`: `panels/common.rs:161-172`; needs-you `●`: `panels/status.rs:152` |
| Dock buttons | Not text or SVG: small window shapes painted from div borders/fills (`panels/status.rs:236-286`); migrate to semantic `dock.*` assets too |

This is an actionable category/callsite inventory, not a claim that all Unicode in the app is decorative. Keep prose punctuation, keyboard symbols (`⌘`, `⇧`, arrows in shortcut labels), terminal/agent output, literal diff `+`/`−`, filenames and user text as text. Do not blindly replace glyphs in source strings or daemon output. Mixed text such as `▶ Start` becomes icon + localized label, not a font-dependent icon embedded in the label. Every icon-only control keeps an accessible name and tooltip.

**Menu bar is a different asset pipeline:** desktop embeds 18/36 px `MenuBarIcon.png` variants, creates an 18-point NSImage and calls `setTemplate(true)` (`desktop/src/macos/status_item.rs:227-230,308-321`). Daemon uses the same PNG pair and template behavior (`daemon/src/menu_bar/cocoa.rs:53-56,363-375`). `xtask/src/menubar.rs:1-7,19-21` generates alpha-only silhouette artwork from the mascot. Native macOS tinting, not `Theme.accent`, controls it. See §3.7 for the deliberate boundary.

## 2. Peer study — official sources only

These are format observations, not a proposal to copy peer implementation code. Source pages were read for this spike; branch URLs are mutable.

| Peer | Official evidence and observed contract | Hutan lesson |
|---|---|---|
| **Zed color themes** | [Extension guide](https://zed.dev/docs/extensions/themes), [developing extensions](https://zed.dev/docs/extensions/developing-extensions): extension root `extension.toml` (`id`, `name`, `version`, `schema_version`, `authors`); files under `themes/` (the filename need not literally be `theme.json`). A family JSON has name/author/themes[], each named light/dark with `style`. [v0.2.0 JSON schema](https://zed.dev/schema/themes/v0.2.0.json) includes chrome/editor/terminal/state roles plus syntax styles. [Themes settings](https://zed.dev/docs/themes): mode/system/light/dark selection, real-time picker preview + Enter commit; local JSON in `~/.config/zed/themes`; `theme_overrides` keyed by theme name. | Dotted semantic roles, independent variant selection, previews and sparse overrides are proven. Use stable ids instead of display-name identity. No need for an extension host. |
| **Zed icon themes** | [Official guide](https://zed.dev/docs/extensions/icon-themes), [v0.3.0 schema](https://zed.dev/schema/icon_themes/v0.3.0.json): `icon_themes/*.json` plus `icons/`; family/themes[] with appearance; directory expanded/collapsed, named directories, chevrons, file stems/suffixes→file_icons id→path; paths root-relative to extension, not JSON file. [Appearance](https://zed.dev/docs/appearance) supports independently chosen icon variants. | These are primarily **file/folder** themes, not a wholesale product-icon replacement contract. Hutan's immediate need is product icons; reserve file associations separately. |
| **VS Code color themes** | [Guide](https://code.visualstudio.com/api/extension-guides/color-theme), [contribution point](https://code.visualstudio.com/api/references/contribution-points#contributes.themes): `package.json` `contributes.themes[]` with id/label/uiTheme/path (`vs`, `vs-dark`, high-contrast bases), JSON `colors` and TextMate `tokenColors`, optional `semanticHighlighting`/`semanticTokenColors`. `workbench.colorCustomizations` and `editor.tokenColorCustomizations` apply live. [`contributes.colors`](https://code.visualstudio.com/api/references/contribution-points#contributes.colors) provides light/dark/high-contrast defaults, including references to registered colors. | Semantic color registry with explicit fallback beats proliferating ad hoc view constants. Do not equate UI color ids with syntax token ids. |
| **VS Code semantic tokens** | [Official semantic guide](https://code.visualstudio.com/api/language-extensions/semantic-highlight-guide): ids include namespace/type/class/enum/interface/struct/typeParameter/parameter/variable/property/enumMember/decorator/event/function/method/macro/label/comment/string/keyword/number/regexp/operator; selectors can add modifiers and language. Missing semantic style falls back through the semantic→TextMate scope map; custom token types can inherit supertype styling. | Future Hutan syntax must have a separate capture/token contract. Import only supported base classes; never promise LSP-semantic equivalence from a static theme. |
| **VS Code file icons** | [Official guide](https://code.visualstudio.com/api/extension-guides/file-icon-theme): `contributes.iconThemes[]`; JSON iconDefinitions can be SVG/PNG or fonts; associations for file/folder/name/extension/language, light/highContrast overrides. Filename wins over extension, extension over language; expanded folder falls back to folder; root folder falls back to folder; default `file` icon catches unmatched files; language contributions can supply fallback icons. | Keep product ids distinct from file matching; deterministic precedence and per-id fallback are essential. Avoid fonts for Hutan product icons. |
| **VS Code product icons** | [Official guide](https://code.visualstudio.com/api/extension-guides/product-icon-theme): `contributes.productIconThemes[]`; iconDefinitions map semantic product ids to fontCharacter/fontId, monochrome, color supplied by color theme; native format requires fonts, not SVG. [`contributes.icons`](https://code.visualstudio.com/api/references/contribution-points#contributes.icons) registers ids with defaults; [maintainer issue](https://github.com/microsoft/vscode/issues/119101) explains default-icon registration. | Adopt semantic-id/default separation, not the font dependency. Do not claim Zed/VS Code icon themes can be imported directly into Hutan's product ids. |
| **Ghostty** | [Official themes guide](https://ghostty.org/docs/features/theme): flat config syntax, background/foreground/cursor-color/selection-foreground/selection-background/repeated palette=index=color; named search user themes before installed resource themes; explicit light/dark selection. Built-in themes sourced from [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes), updated weekly. A theme may set **any Ghostty config**, and the guide warns about malicious settings. | Import only an allowlist of colors. Never load a Ghostty theme as Hutan configuration or follow `config-file`/commands. No automatic weekly downloading. |
| **iTerm2 schemes** | [Official collection sample](https://github.com/mbadolato/iTerm2-Color-Schemes/blob/master/schemes/Blue%20Matrix.itermcolors): plist `Ansi N Color`, Background/Foreground/Cursor/Cursor Text/Selection/Selected Text Color dictionaries with RGB real components, Color Space and alpha. | Good terminal-only interoperability format; allow its standard plist DOCTYPE as an inert declaration, never fetch the external DTD or resolve entities. |
| **Warp** | [Official YAML guide](https://docs.warp.dev/terminal/appearance/custom-themes/): name/accent/cursor/background/foreground/details (`darker`/`lighter`), normal+bright named ANSI maps; cursor defaults accent. Discovers `~/.warp/themes` changes; supports optional JPEG background assets and gradients. | TOML is already Hutan's settings language; don't add YAML for native packs. Warp import can be terminal colors + accent only; explicitly decline images/gradients rather than activate asset fetching. |
| **WezTerm** | [Official appearance guide](https://wezterm.org/config/appearance.html): named built-ins, `colors` overrides scheme, standalone TOML `[colors]` in `~/.config/wezterm/colors`, extra color_scheme_dirs, files shadow built-ins; foreground/background/cursor_bg/fg/border/selection_bg/fg, ansi/brights/indexed and other UI roles. Main config may be Lua. | TOML data files are sufficient; do not evaluate `.wezterm.lua`. Do not copy WezTerm's multiplexer-server palette ownership: Hutan already has client defaults + daemon VT state. |
| **Alacritty** | [Official config](https://alacritty.org/config-alacritty.html): TOML, general.import arrays, live config reload; colors.primary, normal/bright/dim, cursor, vi_mode_cursor, selection, indexed_colors, search/hints; symbolic CellForeground/CellBackground supported. [Official theme collection](https://github.com/alacritty/alacritty-theme). | Import the colors table only; don't recursively follow general.import or apply env/shell config. Symbolic cell-relative colors are not ordinary RGB literals. |
| **Orca (local official checkout)** | `/Users/workie/proj/orca-src-git/src/shared/terminal-custom-themes.ts:4-16,43-70,191-219`: custom terminal themes stored as id/name/source/mode/terminal/importedAt/sourceLabel/unsupportedFeatures, bounded to 200; source warp/ghostty/manual. `src/renderer/src/lib/terminal-theme.ts:17-38,63-85`: built-in/imported picker options and fallbacks. `src/main/ghostty/theme-resolution.ts:6-26,67-94`: 256 KiB cap, only color-bearing Ghostty keys, relative-name traversal guard. Chrome remains CSS variables (`src/renderer/src/assets/main.css:170-260`). | Orca **does have terminal schemes/imports**, not evidence of a general external chrome/product-icon pack API. Reuse its safety lessons; Hutan needs broader chrome coverage and first-class pack files. |

### 2.1 Licensing/import policy

A serialization format is not a license grant for every theme encoded in it. Independently implement parsers from documented fields; don't vendor peer source or schemas merely to parse data. Redistribution of converted palettes/assets still needs the original theme's license and notices; the importer records provenance, never invents a permissive license.

- [iTerm2-Color-Schemes LICENSE](https://github.com/mbadolato/iTerm2-Color-Schemes/blob/master/LICENSE): MIT for the **collection**, explicitly says each theme's copyright/license belongs to its author. Do not blanket-relicense all imported themes MIT.
- [Warp theme repository LICENSE](https://github.com/warpdotdev/themes/blob/main/LICENSE): Apache-2.0. Preserve notices, attribution, and modification notice for converted distributed files; check individual theme origins too.
- [Alacritty theme collection LICENSE](https://github.com/alacritty/alacritty-theme/blob/master/LICENSE): Apache-2.0; same per-theme due diligence.
- [WezTerm LICENSE](https://github.com/wezterm/wezterm/blob/main/LICENSE.md): MIT for project, separate font notices; imported schemes originate from multiple collections, not automatically all MIT.
- [VS Code source LICENSE](https://github.com/microsoft/vscode/blob/main/LICENSE.txt): MIT; third-party Marketplace extensions/theme assets retain their own licenses. Do not download/run VSIX code to import a color JSON.
- [Zed repository licensing](https://github.com/zed-industries/zed#licensing): primarily GPL-3.0-or-later, Apache-2.0 where marked. **Do not link/copy Zed theme crates into Hutan**. Independently read documented JSON; check each extension/theme license before redistributing any converted content. A public schema URL alone does not establish a separate permissive schema license.
- Orca checkout `LICENSE:1-13`: MIT, copyright Lovecast Inc. This proposal copies no Orca code.
- [Phosphor core LICENSE](https://github.com/phosphor-icons/core/blob/main/LICENSE): MIT, retain copyright/license with embedded SVGs. Existing Lucide licensing is ISC (`crates/desktop/src/icons.rs:1-3`, `assets/icons/LICENSE-lucide.txt`). Keep notices for any retained assets; remove obsolete Lucide artwork/notices only when no longer shipped.

Import metadata may use `license = "LicenseRef-Unknown"` and a diagnostic “license not established; private use only, verify before sharing”. This is **not** a redistribution license. Native author scaffolds ask for author/license explicitly; existing local files can load with that unknown marker. Shipping a built-in always requires a reviewed actual SPDX expression and applicable license texts. This is engineering policy, not a legal opinion.

## 3. Hutan pack contract

Everything below is a **proposed design**. Proposed paths/types/commands are not claims that they exist today.

### 3.1 Package layout and identity

Use one pack per directory:

```text
<HUTAN_HOME or resolved data-dir>/
  settings.toml
  themes/
    canopy-night/
      theme.toml
      LICENSE                 # recommended for sharing; required for bundled third-party work
  icon-themes/
    canopy-lines/
      icon-theme.toml
      icons/
        search.svg
        close.svg
      LICENSE
```

Allow a **single-file color theme** `themes/canopy-night.toml` as shorthand for the same manifest (no relative assets). Icon themes always use a directory because assets must have an unambiguous confinement root. No archives, arbitrary search paths, tilde/environment interpolation, include directives or executable extension manifests. Discovery is exactly one directory level for packs; SVG asset subdirectories may be at most depth 4 below that pack.

Metadata at TOML root:

| Key | Contract |
|---|---|
| `schema_version` | Required integer `1`. Unsupported future versions disable pack with diagnostic; no best-effort interpretation. |
| `id` | Required stable ASCII lower-kebab id, `[a-z][a-z0-9]*(?:-[a-z0-9]+)*`, 1–64 bytes, independent of filename/display name. `auto` is reserved. No `:` in author ids. |
| `name` | Required display label, 1–120 Unicode characters; reject controls. Not selection identity. |
| `authors` | Required nonempty string array, each 1–200 characters; reject controls. |
| `license` | Required SPDX expression or `LicenseRef-*` marker, at most 200 characters. `license_file` optionally names a confined local text file. |
| `version` | Optional semver string, metadata only (not schema negotiation). |
| `description` | Optional plain text, ≤1,024 characters; no HTML/markdown execution. |
| `repository` | Optional HTTPS URL shown as inert text until user explicitly opens it. Never fetched by discovery. |
| `appearance` | Required color-pack enum `dark` or `light`. Icon pack optional `dark` or `light`; omitted means polarity-independent currentColor artwork. |
| `root` | Optional boolean, default false. `true` requires complete baseline coverage and forbids `extends`; supported for embedded, exported and independently authored complete packs. Root icon packs must cover every registered id. |
| `extends` | Required for sparse packs: `builtin:hutan-dark` / `builtin:hutan-light`, matching `appearance`; icon base `builtin:phosphor`. Complete `root = true` packs omit it. |

Use `id = "hutan-dark"` for the embedded pack's public selection id; `builtin:hutan-dark` is a **resolver reference**, not an author id or filename. Permit user packs with the same public id to shadow embedded built-ins, visibly labelled “user override”; `builtin:` still addresses immutable embedded content, so extending your own shadow id cannot cycle. Duplicate ids within the user tier (including folder+single-file) invalidate all user candidates for that id with both paths in diagnostics; the embedded root is still available. Never choose by filesystem enumeration order. Other custom→custom inheritance is deliberately unsupported: no dependency install graph, cycles or disappearing parent packs. Sharing a derivative needs only the named built-in and its sparse manifest.

**Project-local packs:** do not discover `.hutan/themes` or `.hutan/icon-themes` automatically in TH-1–4. A repository must not silently restyle application-wide approval surfaces or read arbitrary SVGs just by being opened. Authors can keep packs anywhere, validate them by path, preview explicitly, and copy/import into the user directory. Selection/overrides are user-only; current `terminal.theme` remains workspace/project-overridable because that behavior exists. A later trusted-project feature may add `.hutan/themes/<id>/theme.toml` with explicit per-root opt-in and window scope; it is not necessary for extensibility and is not silently reserved as an active source.

### 3.2 Token names, types and resolution

`[colors]` is a flat map with **quoted dotted semantic keys**, e.g. `"ui.sidebar.background" = "#1f1e1d"`. Same key grammar as icon ids: `^[a-z][a-z0-9]*(?:[.-][a-z0-9]+)*$`, max 96 bytes. Dots are semantic segments; hyphens separate words. No snake_case aliases in manifests, no arbitrary nested `colors.ui` tables. Rust fields can keep conventional snake_case; one exhaustive registry maps ids to typed fields/documentation/fallbacks. Short names `fg/muted/border/surface/accent/success/warning/error` in Chrome V3 design are internal role aliases, not a competing author-facing spelling.

Generate versioned JSON Schema projections for TOML author tooling from the same Rust manifest/token registry, with descriptions, enum values, value bounds and documented optional-role fallbacks. Commit these schema artifacts with the implementation and support local/offline schema association; validation never downloads a schema. Schema examples must round-trip through the actual TOML loader. Runtime cross-field/path/SVG/contrast checks supplement schema validation; do not maintain a second manually divergent list of tokens.

Colors: `#RRGGBB` or `#RRGGBBAA`, case-insensitive on input, lower-case on export. RGB is sRGB, alpha is straight alpha. All terminal core/ANSI colors and text foregrounds must be opaque; translucent surface/effect/diff backgrounds and borders are supported. No expressions, CSS color functions, arbitrary references, interpolation or OS API selectors. This is a small deterministic schema, not a stylesheet language.

Resolution order:

1. Load embedded built-in **declarations** and user declarations; validate metadata/types/security before publication.
2. Choose the immutable embedded base named by custom `extends`; merge only known explicitly declared values from custom pack.
3. Apply matching **user settings** overrides, then scoped preview overrides. A bad override rejects that override update, keeping its previous accepted values; it must not poison the underlying pack.
4. Compute optional semantic defaults from the resulting core values; then apply explicitly supplied optional-role values. This matters: changing `state.success` should also recolor the default `diff.added`, not leave a stale built-in-resolved green. Keep declarations until this step, not just inherited pre-resolved RGBs.
5. Produce immutable typed `ResolvedTheme` with all paint/layout values filled. Renderers never look up strings, parse TOML, touch disk or allocate color maps per frame.

All **sparse-pack** colors/metrics/fonts are optional because `extends` supplies them. Every root pack must explicitly supply all 17 core UI colors (the 16 fields above plus on-accent), all 20 terminal colors (4+16 ANSI), `ui.shadow`/`ui.scrim` effect colors, font defaults, both density tables and shared geometry. Derived optional role fallbacks are schema algorithms, not another hidden hard-coded palette. Reject partial/invalid roots; embed a prevalidated built-in root snapshot for startup error containment, generated from the same source manifest rather than maintained as another palette table.

Canonical required core color keys are the table in §1.1 plus `ui.on-accent`; terminal keys are `terminal.foreground`, `terminal.background`, `terminal.cursor`, `terminal.selection.background`, and `terminal.ansi.{black,red,green,yellow,blue,magenta,cyan,white,bright-black,bright-red,bright-green,bright-yellow,bright-blue,bright-magenta,bright-cyan,bright-white}`. Terminal background need not equal UI background in a custom pack; equality is only the built-in design choice.

Optional roles and fallback contract:

| Optional token | Default after core merge |
|---|---|
| `ui.sidebar.hover` | ui.foreground at alpha 0.08 |
| `ui.scrim` | built-in effect color for chosen appearance (customizable literal) |
| `ui.shadow` | built-in effect color for chosen appearance (customizable literal) |
| `ui.drop-target` | ui.accent at alpha 0.28 dark / 0.22 light |
| `state.working`, `state.starting` | ui.accent |
| `state.idle`, `state.unknown`, `state.interrupted`, `state.disconnected` | ui.text.muted |
| `state.blocked`, `state.approval`, `state.question`, `state.needs-you` | state.warning |
| `state.completed` | state.success |
| `state.failed` | state.error |
| `diff.added`, `diff.removed`, `diff.modified`, `diff.renamed` | state.success / state.error / ui.accent / state.warning |
| `diff.added.background`, `diff.removed.background` | corresponding diff color at alpha 0.16 |
| `diff.hunk.background`, `diff.hunk.foreground` | ui.accent at alpha 0.10 / ui.text.muted |
| `terminal.cursor.foreground` | terminal.background |
| `terminal.selection.foreground` | absent = preserve each cell's foreground (typed `Option<Rgb>`, not a magic color string) |
| `terminal.block.success`, `.error`, `.neutral`, `.accent` | state.success / state.error / ui.text.muted / ui.accent |
| `terminal.block.separator` | mix terminal.background toward terminal.foreground by 0.16 |
| `terminal.block.highlight`, `.hover`, `.folded` | block.accent alpha 0.13 / block.neutral alpha 0.07 / mix terminal bg toward fg by 0.07 |
| `markdown.link`, `markdown.inline-code.background` | ui.accent / ui.surface.hover |

State semantics stay in the session model. Existing `Tone` adapter is the generic palette fallback; presentation of a specific state can select its optional state role. `state.needs-you` applies to the aggregate indicator only, never forces failed and blocked cards into the same color. No new mandatory V3-only roles are required.

Unknown token/icon ids warn and are ignored (supports forward-compatible author packs); unknown structural tables/keys also warn. Wrong type, malformed known color, invalid appearance/base, invalid required metadata or a resource/security violation is an **error**, not a silent ignore. A color pack is published atomically only if error-free. An icon pack can publish valid per-id entries while an invalid asset/id falls back to the default, but parse/metadata/base errors reject the whole update. Diagnostics distinguish “kept previous pack” from “using default icon for this id”. TOML duplicate keys are parse errors.

### 3.3 Selection and user overrides

Keep `appearance.appearance` as the single polarity/mode selector (do not introduce a second `mode` that can disagree). Add:

```toml
[appearance]
appearance = "system"             # existing system | light | dark
density = "compact"               # existing compact | comfortable
theme = { light = "hutan-light", dark = "canopy-night" }
icon_theme = "canopy-lines"        # independent; default phosphor

[appearance.theme_overrides."canopy-night".colors]
"ui.accent" = "#d99a58"

[terminal]
theme = "auto"                     # existing: now uses active color pack's terminal palette
```

`appearance.theme.light` must point to a light pack; `.dark` to a dark pack. Unknown/wrong-polarity selection shows a diagnostic and resolves to embedded root of that polarity; retain the requested selector so fixing/installing it recovers automatically. A pinned mode selects that slot, not a different theme's declared appearance. Icon pack with explicit appearance must match active polarity or falls back with diagnostic; ordinary monochrome packs omit appearance and work in both.

`terminal.theme = "auto"` now means the active UI pack's terminal section; an explicit color-pack id picks only its terminal section without changing chrome, preserving existing pinned-terminal usage. The old `hutan-dark`/`hutan-light` ids stay valid because they remain built-in public ids, not compatibility shims. Remove obsolete hard-coded resolvers after migrating every caller. Existing explicit terminal font family/size/line-height and runtime zoom win over pack defaults; to make pack defaults effective, resolve user-declared keys against provenance rather than treating serialized `TerminalSettings::default` as an explicit user choice. Font and UI scale settings are user preferences; a pack cannot change shell, environment, keymap, layout membership, permissions, Reduce Motion or any daemon behavior.

The existing settings namespace is app-wide/non-workspace-overridable (`AppearanceSettings` does not opt in; contrast `TerminalSettings`), so theme picks/overrides follow that policy. A future per-project UI theme needs window-local style ownership first; do not accidentally make project TOML override current globals.

### 3.4 Fonts, density and effects are data too

Native TOML tables: `[fonts]` (`ui`, `prose`, `mono` string arrays of installed family names), `[density.compact]`, `[density.comfortable]` (flat quoted dotted numeric keys), `[metrics]` (shared geometry/effect numbers), `[provenance]` (import-only inert metadata).

Density keys and current values map exactly:

| Key | Compact / comfortable |
|---|---|
| `sidebar.row-height`, `tab.height`, `pane.header-height`, `status-bar.height` | 26/30, 30/34, 30/36, 24/24 |
| `spacing`, `radius.control`, `radius.card` | 4/6, 6/8, 10/12 |
| `text.body.size`, `text.secondary.size`, `text.section.size` | 12/13, 11/12, 11/11 |
| `text.prose.size`, `text.prose.line-height`, `control.height` | 15/15, 1.6/1.6, 28/28 |
| `terminal.padding`, `terminal.block-gutter` | 8/12, 3/4 |

Shared `[metrics]`: `titlebar.height=36`, `radius.pill=999`, `reading.width=720`, `mascot.size=96`, `nav.text.size=13`, `nav.icon.size=16`, `nav.line-height=18`, `nav.padding-x=8`, `nav.padding-y=6`, `nav.gap=2`, `strip.button.size=28`, `strip.gap=4`, `terminal.font-size=13`, `terminal.line-height=1.2`, `shadow.offset-x=0`, `shadow.offset-y=8`, `shadow.blur=24`, `shadow.spread=0`. The root appearance packs explicitly carry these defaults. No arbitrary per-view layout/style rules: geometry that defines the workbench's behavior (dock placement, split ratios, hit testing, animation enablement) remains in settings/state/code, not a theme.

Validation bounds: finite numbers only; ordinary geometry 0–1200 logical px (positive heights, text sizes 8–40 for UI; terminal 4–72; widths reading 240–1600), radius 0–1000, line-height 0.8–3, shadow offset −128–128, blur/spread 0–128, block-gutter 0–16. Validate coupled constraints: row/control/header heights must fit resolved text line height+vertical padding; nav line-height must fit font size, no negative content dimensions. Prefer errors to silently clamping pack data. Existing explicit user font limits stay as they are. Missing installed families warn then try next name, finally system UI / Georgia or UI / Menlo. Preserve New York registration as a platform font resolver, not a file path exposed to packs. No pack-supplied TTF/OTF/WOFF loading.

### 3.5 Discovery, bounds and hot reload

Extend the existing settings-layer infrastructure in `hutan-settings`; do not add a watcher per view or per icon. One registry per application/data directory; CLI uses the same pure parser/validator without starting a watcher or daemon. Proposal: `ThemeRegistry` publishes `Arc<ThemeCatalogSnapshot>` (descriptors, validated declarations, typed resolved themes, diagnostic set, monotonically increasing generation). Desktop converts colors to GPUI only at snapshot application. A registry update coalesces to the latest generation; no unbounded event queue.

- Bounds: 256 color-pack candidates + 256 icon-pack candidates; manifest ≤256 KiB and ≤16 MiB total manifest bytes per catalog; metadata strings bounded above; ≤512 color/metric entries; icon map ≤512 entries; SVG ≤64 KiB each and ≤8 MiB per icon pack; at most 512 files per pack, asset directory depth 4; ≤512 diagnostics retained plus one truncation summary. Cap retained validated raw SVG bytes at 32 MiB across active/preview/default generations; inactive asset validation processes one pack at a time and retains hashes/status, not every pack's raw asset bytes. Bound the separate decoded/raster cache to 64 MiB with LRU eviction and current-frame references; don't retain all 256×8 MiB packs in memory. Count all candidate files during bounded scanning, not only successful entries. Hit a cap → visible diagnostic; deterministic sorted discovery; no recursive walk outside allowed roots.
- One `notify` watcher, bounded channel 64, debounce 100 ms/max-delay 500 ms, reused mechanics from current watcher. Watch pack roots for creation/removal and only discovered allowed subdirectories, rather than recursively watching entire project/home. Missing roots: watch the existing parent and re-register on creation. Atomic-save rename, directory rename and delete/recreate must work.
- Scan/parse/XML processing off the UI thread with a single worker, one pending rescan flag; overflow coalesces to a rescan, never blocks the OS callback. Check actual read length even after metadata size checks; bounded reads reject growing files.
- Compare source content hashes/generations; unchanged source produces no repaint. Parse only changed manifests/assets; no watcher-triggered network. Active theme's changes rebuild affected resolved snapshot, invalidate chrome/chat/diff/terminal styles once; inactive pack changes update picker/diagnostics, not all terminal rows. The worker verifies path confinement again for every reload.
- Bad edit: keep the previous complete accepted color pack; initial invalid pack cannot be selected; built-in fallback remains available. Missing selected pack: fall back visibly, retain requested id, recover on reappearance. Icon bad asset: retain last-good entry on an edit, otherwise default per id. Never drop the whole workbench to blank/mixed partially updated colors.
- SVG asset keys must include pack identity + content generation/hash (e.g. `theme-icons/<pack>/<hash>/<semantic-id>.svg`), not just the semantic id or disk path. Otherwise GPUI's SVG cache can keep stale bytes after an edit. `AssetSource` serves only validated in-memory bytes, no arbitrary `file://` resource lookup. Bound retired generations/caches, retaining only those still referenced by a frame; swap and release old snapshots.

### 3.6 Icon theme contract and author guidance

Independent `icon-theme.toml` metadata as §3.1, `extends = "builtin:phosphor"`, optional polarity, and `[icons]` flat quoted semantic id→relative SVG path. No pack-defined colors: rendering location supplies the resolved color role. Omitted ids fall back **per id** to immutable built-in Phosphor; there is no missing-glyph box, no transparent omission meaning “hide button”, no reference to another custom icon theme. Unknown ids warn and are ignored. Same source SVG can implement multiple ids.

ChromeV3Design owns the exhaustive semantic id→Phosphor selection and usage inventory in [Chrome v3 §5.2–5.3](2026-10-20-chrome-v3.md#52-every-existing-icon-variant--semantic-role--default-file); its namespace contract is `panel.*`, `nav.*`, `action.*`, `state.*`, `status.*`, `session.*`, `vcs.*`, `dock.*`, `harness.*`, `brand.*`. The map uses official Phosphor revision `2b75f3ad12b420c9504ef05df8d2564a28f8500e` (provenance evidence in that spike §5.1), with Regular default and optional whole-pack Bold variant. This document owns spelling grammar, package layout, resolution and safety, not a competing icon list. TH-2 must use that canonical list verbatim and ensure every registered id has a built-in asset. Runtime Rust enum/typed id maps to a stable semantic string, **not** a vendor filename. Register panel icons as typed ids and remove the `SharedString` decorative-glyph fallback. Icon set choice must not force a color theme, and vice versa.

Guidance: author vector viewBox (prefer 24×24; native Phosphor 256×256 is accepted/scaled), square aspect ratio, no intrinsic sizing that changes the control, enough internal padding to read at 14/16 logical px. Render 14 for compact inline controls, 16 for nav/strips, with hit targets determined by UI metrics, not SVG dimensions. For stroked designs aim at optical weight equivalent to 1.5–2 px on a 24-unit canvas; use Phosphor Regular by default and optional whole-pack Bold, not selected-state Fill or automatic stroke rewriting. Selection uses color/surface, not geometry swapping. Use only `fill="currentColor"`/`stroke="currentColor"` or `none`, rounded strokes where the design calls for them. Accept pre-expanded filled paths too: Phosphor must not be rejected just because it is not a Lucide-style stroke. No colored brand images in this monochrome contract; custom brand mark may be monochrome, while rich mascot illustrations remain separate product assets. Directional assets are authored for their semantic direction: normalize the built-in `dock.right` mirror into its safe SVG geometry; never blindly mirror a user-supplied right-dock SVG at render time.

Optional per-file-type icons are **later**, under a separate versioned association table and registry, not arbitrary new product ids. Recommended precedence: exact filename > longest compound suffix > language id > default file; explicit expanded folder > collapsed folder > default folder. They need a multicolor asset policy and editor/file-tree consumers first. No VS Code font product-icon importer in TH-4.

### 3.7 Application boundary, native surfaces and CLI

ADR 0001 (`docs/adr/0001-daemon-authoritative-topology.md:17-30`) makes clients render and daemon own execution; ADR 0010 (`docs/adr/0010-desktop-workbench-and-settings.md:28-41`) makes presentation settings desktop-owned and forbids GPUI in settings/service crates. This proposal conforms:

- Parser/catalog/validation/import/export are GPUI-free in `hutan-settings`; `desktop` applies chrome/fonts/icons and `terminal_view` consumes typed palettes/overlay styles. No `daemon`, `protocol`, `core`, storage migration, remote settings synchronization or permission changes for native theme packs.
- Keep the **existing** client-report-default-colors route for terminal OSC replies; no manifest/path/author/asset bytes reach the daemon. VT application OSC overrides still win until reset. Two clients can paint differently under existing protocol semantics; don't solve multi-client color-query arbitration in this theme slice.
- Native traffic lights, macOS menus, Dock app icon and notification icon remain system/product identity. The daemon's menu bar item continues using embedded template artwork, even with no desktop connected. Desktop menu bar uses the same fixed artwork to avoid a visible identity mismatch on daemon/desktop handoff. **`brand.menu-bar` is not a supported override in TH-2**; diagnose it as unsupported if present. This is an explicit scope boundary, not an unnoticed hard-coded icon: allowing it in both processes would require an additional platform/native branding contract and would contradict “daemon untouched”. The internal desktop brand/mascot mark, if part of V3's monochrome catalog, remains independently themeable; colorful mascot illustrations are not SVG product icons.
- CLI management commands operate on local files only, do not autostart `hutand`, and do not need GPUI. Ordinary `hutan ls`, logs, machine-readable JSON and `hutan attach` remain host-terminal/NO_COLOR behavior, not recolored from desktop packs. A CLI preview is textual swatches/diagnostics, never sends OSC palette changes to the invoking terminal unless a separate explicit feature is approved.

### 3.8 Validation, diagnostics, accessibility and security

**Diagnostics:** extend/reuse `SettingsError` severity/file/key/message plus an optional source span if available; expose in Settings Problems and CLI text/JSON. Include exact token/icon key and actionable reason (“icons/action.close: symlink component refused”; “colors.ui.border: expected #rrggbb[aa]”). An invalid pack is listed disabled with its errors so users can fix it. Registry I/O failures become diagnostics, not panics. Watcher failure shows a warning and an explicit Reload action; don't pretend reload succeeded. Never log entire manifests/SVG contents or unrelated file contents.

**Contrast:** warning-only for user themes; don't “repair” chosen colors or prevent access to a custom theme. Compute standard sRGB relative luminance with transfer breakpoint 0.04045 and contrast `(lighter+.05)/(darker+.05)`, compose alpha over each documented actual background first. [WCAG 2.2 AA text](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html) is ≥4.5:1 ordinary text and ≥3:1 qualifying large text, without rounding up; [non-text controls](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html) ≥3:1. Check foreground/muted/placeholder against main/sidebar/right-sidebar/surface/hover/selected backgrounds, on-accent vs accent, state text/icon roles vs their actual fills, diff text vs tinted rows, terminal fg/selection/cursor glyph; report sampled ANSI fg/background contrast too. Do not assert arbitrary ANSI/truecolor application output is AA: the application chooses combinations. Decorative borders, disabled controls and a palette's opposite-polarity paper neutral are not automatic text-AA failures. Built-ins need a reviewed baseline of warnings (including currently white-on-orange button combinations), not suppression by renaming text “decoration”. Include role pair/ratio/threshold and a reset-to-built-in escape hatch; no accessibility claim without native review.

**Data-only threat model and enforced limits:**

1. No script execution, plugins/WASM, commands, dynamic config, CSS, network fetches, shell expansion or embedded credentials. Ignore no unsafe foreign config field merely because it happens to parse as TOML. SVG is untrusted input even though today's assets are embedded.
2. Paths: only normalized relative UTF-8 paths, no absolute/drive/UNC roots, backslashes, `..`, empty components, NUL, URL schemes, `~` expansion or symlinks. Resolve from pack root, use descriptor-relative no-follow opening of every component and verify regular file; canonicalize-only-then-open is insufficient against races. The *configured data root* can be resolved once using the application's normal path policy; no descendant symlink traversal. Apply same confinement to licenses/import includes. Write output/scaffolds with no-clobber atomic creates; export must not escape destination through symlinks.
3. Parse SVG with a strict XML parser, external entity/DTD resolution disabled. Reject DOCTYPE/entity declarations for SVG, processing instructions, scripts, `on*` event attributes, foreignObject, text/fonts, image, style/CSS, animation, filter, mask, pattern, `use`, href/xlink, all `url(...)`, external/data/file references. Whitelist svg/g/path/rect/circle/ellipse/line/polyline/polygon and numeric geometry/transforms, viewBox, currentColor/none, fill-rule/clip-rule, opacity, linecap/join/stroke-width. Reject rather than silently strip unsupported elements. Normalize safe bytes and only then pass to GPUI; no raw source ever enters its asset loader.
4. Bound complexity as well as bytes: XML depth ≤16, ≤2,048 elements, ≤16,384 parsed path segments, per-attribute length ≤32 KiB, finite coordinates with absolute value ≤1e6, positive finite square viewBox with side ≤4096. SVG decoder/raster cache output dimensions follow requested bounded UI size, not attacker width/height. Enforce wall/CPU work limits through bounded input/complexity and one worker; fuzz XML/path parser and rejection cases.
5. Importers never evaluate Lua/JS, execute VSIX activation, follow remote `include`, load font binaries, resolve XML network DTDs or import Ghostty non-color config. No auto-scan of other apps' configuration or user libraries: user chooses a source file/folder. All parsing and pack enumeration stays within specified byte/count/depth limits.
6. Installer is optional later (§5). Git URLs must not quietly introduce hooks, submodules, credential prompts or arbitrary protocols; no package install side effects in ordinary theme discovery.

## 4. Complete example manifests

These are complete **valid proposed-schema examples**, not claims that the loader exists. The theme example explicitly lists every current core color and all typography/density/shared metric defaults; it extends a built-in, so removing any optional override still yields a complete resolved theme. Effect/state/diff/overlay defaults resolve by §3.2. The icon manifest is complete as a sparse pack: only the declared two SVG assets are required; every other id falls back. Sample SVG contents below make it independently authorable without fetching Phosphor.

### 4.1 `themes/canopy-night/theme.toml`

```toml
schema_version = 1
id = "canopy-night"
name = "Canopy Night"
authors = ["Hutan Theme Authors"]
license = "MIT OR Apache-2.0"
version = "1.0.0"
description = "Warm dark chrome and terminal palette, editable without recompilation."
appearance = "dark"
extends = "builtin:hutan-dark"

[colors]
"ui.background" = "#262624"
"ui.sidebar.background" = "#1f1e1d"
"ui.sidebar.selected" = "#2e2d2b"
"ui.sidebar-right.background" = "#1a1918"
"ui.sidebar-right.selected" = "#2a2927"
"ui.surface" = "#30302e"
"ui.surface.hover" = "#3a3936"
"ui.border" = "#ffffff12"
"ui.foreground" = "#f5f4ef"
"ui.text.muted" = "#a6a49c"
"ui.text.faint" = "#6f6d66"
"ui.accent" = "#ea7a43"
"ui.accent.soft" = "#3e2c23"
"ui.on-accent" = "#ffffff"
"state.success" = "#6bbf85"
"state.warning" = "#e2ae45"
"state.error" = "#e87562"
"ui.shadow" = "#00000066"
"ui.scrim" = "#00000080"
"terminal.foreground" = "#eae8e1"
"terminal.background" = "#262624"
"terminal.cursor" = "#ea7a43"
"terminal.selection.background" = "#4b3b30"
"terminal.ansi.black" = "#4a4944"
"terminal.ansi.red" = "#e87562"
"terminal.ansi.green" = "#6bbf85"
"terminal.ansi.yellow" = "#e2ae45"
"terminal.ansi.blue" = "#72a5ec"
"terminal.ansi.magenta" = "#c996d8"
"terminal.ansi.cyan" = "#5fc2c6"
"terminal.ansi.white" = "#d6d4cc"
"terminal.ansi.bright-black" = "#7a7870"
"terminal.ansi.bright-red" = "#f08c7a"
"terminal.ansi.bright-green" = "#86d19c"
"terminal.ansi.bright-yellow" = "#eec261"
"terminal.ansi.bright-blue" = "#8fb9f2"
"terminal.ansi.bright-magenta" = "#daaee6"
"terminal.ansi.bright-cyan" = "#7dd3d6"
"terminal.ansi.bright-white" = "#f5f4ef"

[fonts]
ui = [".SystemUIFont"]
prose = [".New York", "Georgia", ".SystemUIFont"]
mono = ["Menlo", "monospace"]

[density.compact]
"sidebar.row-height" = 26
"tab.height" = 30
"pane.header-height" = 30
"status-bar.height" = 24
spacing = 4
"radius.control" = 6
"radius.card" = 10
"text.body.size" = 12
"text.secondary.size" = 11
"text.section.size" = 11
"text.prose.size" = 15
"text.prose.line-height" = 1.6
"control.height" = 28
"terminal.padding" = 8
"terminal.block-gutter" = 3

[density.comfortable]
"sidebar.row-height" = 30
"tab.height" = 34
"pane.header-height" = 36
"status-bar.height" = 24
spacing = 6
"radius.control" = 8
"radius.card" = 12
"text.body.size" = 13
"text.secondary.size" = 12
"text.section.size" = 11
"text.prose.size" = 15
"text.prose.line-height" = 1.6
"control.height" = 28
"terminal.padding" = 12
"terminal.block-gutter" = 4

[metrics]
"titlebar.height" = 36
"radius.pill" = 999
"reading.width" = 720
"mascot.size" = 96
"nav.text.size" = 13
"nav.icon.size" = 16
"nav.line-height" = 18
"nav.padding-x" = 8
"nav.padding-y" = 6
"nav.gap" = 2
"strip.button.size" = 28
"strip.gap" = 4
"terminal.font-size" = 13
"terminal.line-height" = 1.2
"shadow.offset-x" = 0
"shadow.offset-y" = 8
"shadow.blur" = 24
"shadow.spread" = 0
```

RGBA8 encodes 7% border as alpha 18/255 (~0.07059), 50% scrim as 128/255 (~0.50196). That small quantization is an explicit cutover choice; root pack tests should compare decoded bytes/tolerances, not require unrepresentable exact 0.07 in an 8-bit alpha format. Do not import the unused opaque dark-border table slot.

### 4.2 `icon-themes/canopy-lines/icon-theme.toml`

```toml
schema_version = 1
id = "canopy-lines"
name = "Canopy Lines"
authors = ["Hutan Theme Authors"]
license = "MIT OR Apache-2.0"
version = "1.0.0"
description = "Two custom product icons; all other semantic ids use Phosphor."
extends = "builtin:phosphor"
# No appearance: currentColor artwork works in both modes.

[icons]
"nav.search" = "icons/search.svg"
"action.close" = "icons/close.svg"
```

`icons/search.svg`:

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
  <circle cx="10.5" cy="10.5" r="6.5"/>
  <path d="M15.5 15.5 21 21"/>
</svg>
```

`icons/close.svg`:

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round">
  <path d="M6 6 18 18M18 6 6 18"/>
</svg>
```

## 5. Settings experience, CLI and interoperability

### 5.1 Settings pickers and preview

Appearance page has mode, light-theme, dark-theme, icon-theme and density selectors plus author/license/source, “Open pack folder”, “Validate / Reload”, “Duplicate as new pack”, “Reset to embedded default”. Search by name/id/author. Label Embedded/User override/Imported and disabled invalid packs distinctly. Existing terminal-theme picker uses the same registry, with Auto (active appearance theme) and all valid color-pack ids rather than only built-ins.

Selection previews live while keyboard-navigation/hovering explicitly in picker, without persisting on every move. Enter/Apply writes the user selection once; Escape/outside dismissal cancels and resolves the **current** persisted selection (not a stale saved snapshot if the file or system mode changed meanwhile). Settings close/app quit cancels uncommitted preview. For icon preview show representative nav/action/state/dock grid, including an obvious fallback example. Preview colors on sample chrome, state cards, diff and terminal ANSI row; show warnings in context. A deliberately unreadable theme must never remove the accessible “Reset theme” command/picker escape path. Keep preview scoped to workbench or a preview subtree, not a process-global `UiStyle` mutation affecting unrelated windows. Committed app-wide selection updates all windows consistently.

### 5.2 CLI surface (local, no daemon)

Use one `theme` command family with `--kind color|icon` (default color), rather than two incompatible command trees:

```text
hutan theme ls [--kind color|icon] [--json]
hutan theme validate <path-or-id> [--kind color|icon] [--json] [--strict]
hutan theme new <id> --name <name> --author <author> --license <spdx>
                [--extends builtin:hutan-dark] [--kind color|icon] [--output <dir>]
hutan theme export <id> --output <dir> [--kind color|icon] [--resolved]
hutan theme import <file> --from ghostty|iterm2|alacritty|vscode|zed|wezterm|warp
                   --id <id> --name <name> [--appearance dark|light]
                   [--variant <name>] [--output <dir>] [--dry-run]
```

`ls` shows id/name/appearance/origin/validity and shadowing; JSON is versioned output. `validate` checks manifest, required inherited resolution, all referenced assets, contrast and licensing diagnostics using exactly the desktop rules. Exit 0 success including warnings, 1 errors/I/O, 2 invalid invocation; `--strict` makes warnings exit 1 but still does not change rendering behavior. `new` produces a complete sparse pack with explicit base and real metadata; icon new includes one safe example SVG, default color pack needs only a small `[colors]` override plus the base. Refuse overwrite by default. `export` writes editable native declarations + all needed assets/license notices; `--resolved` flattens current fallbacks and metadata/provenance into a self-contained root export accepted only when complete. This root-export exception to custom-required-extends is enabled by explicit `root = true`, complete schema coverage, and never permits cross-pack imports. Embedded packs use `root = true` too. Export does not serialize temporary preview unless explicitly requested through the GUI's “Export preview”. A path argument is detected syntactically (separator/existing path); add explicit `--path`/`--id` disambiguation in clap to avoid a pack-id/file-name collision.

**Contract clarification:** root manifests require `root = true` and no `extends`; sparse custom manifests require `extends` and omit/root=false. Both are the same native format. A complete independently-authored pack may choose root=true; it must supply the entire required baseline. Root mode is not inferred from missing extends. Thus dogfooding/export does not create a private privileged format.

Import parses first into an inert intermediate document, reports mapped/unmapped fields and warnings, previews before writing, and writes a new native pack atomically only on explicit import. No import mutates the original source, installs an extension, edits active selection implicitly or asserts unsupported syntax/assets worked. `--dry-run` performs no filesystem writes. Metadata records source format/name, source file content SHA-256, declared upstream repository/license if known, and conversion warnings; no unnecessary absolute source path in a shared export.

### 5.3 Exact initial importer mapping

Target names below are §3.2 canonical color keys. “Terminal core” means foreground/background/cursor/selection background plus normal and bright ANSI. Every importer starts from a selected polarity's Hutan root; missing source fields inherit, with an import summary. Infer polarity by source metadata or background luminance (label inference); explicit `--appearance` wins. If only terminal colors exist, **do not fabricate an entire coordinated chrome palette**: preserve built-in chrome. Offer a separate explicit `--match-chrome` option mapping only ui.background/foreground to terminal bg/fg, with a warning that other surfaces remain inherited.

| Input | Fields mapped | Explicit limits / warnings |
|---|---|---|
| Ghostty theme text | `foreground`, `background`, `cursor-color`, `cursor-text`, `selection-background`, `selection-foreground`, `palette = 0..15=...` → corresponding terminal keys; cursor-text→cursor.foreground | Ignore/report all non-color config; never follow config-file. palette 16–255, bold-color, split-divider and non-RGB syntax not supported initially: report, not silently drop. Repeated palette indexes last assignment wins in source order. |
| iTerm2 `.itermcolors` XML/binary plist | `Foreground Color`, `Background Color`, `Cursor Color`, `Cursor Text Color`, `Selection Color`, `Selected Text Color`, `Ansi 0 Color`…`Ansi 15 Color`; component doubles×255 rounded nearest with finite [0,1] check | Convert known sRGB; missing Color Space is legacy sRGB with warning; reject unsupported color spaces unless an explicit color-management conversion exists. No DTD fetch/entity expansion; the standard plist DOCTYPE is inert. Bold/badge/link/cursor-guide colors and alpha on opaque roles report unsupported. No profile commands. |
| Alacritty TOML | colors.primary.foreground/background; colors.normal/bright named eight colors; colors.cursor.cursor/text; colors.selection.background/text | Exact RGB values; selection text `CellForeground`→omit override (preserve each cell foreground). Other cell-relative symbols, including cursor text `CellBackground`, are not equivalent to a fixed terminal background when an application paints colored cells: warn and use inherited Hutan default rather than claim a lossless mapping. dim/bright_foreground/indexed/vi/search/hint fields report unsupported; no recursive general.import. Legacy YAML not accepted under `--from alacritty` initially; suggest its TOML scheme or explicit Warp parser when truly Warp YAML. |
| VS Code color JSON/JSONC | colors: editor.background/foreground→ui.background/foreground; sideBar.background→ui.sidebar.background; panel.background→ui.sidebar-right.background; list.activeSelectionBackground→ui.sidebar.selected; editorWidget.background→ui.surface; list.hoverBackground→ui.surface.hover; panel.border→ui.border; descriptionForeground→ui.text.muted; disabledForeground→ui.text.faint; focusBorder→ui.accent; button.background/foreground→ui.accent/on-accent (button takes precedence over focusBorder); errorForeground→state.error; terminal.foreground/background/cursor.foreground/selectionBackground/selectionForeground and ansiBlack…ansiBrightWhite→terminal keys; diffEditor.insertedTextBackground/removedTextBackground→diff backgrounds | Multiple UI concepts collapse to fewer Hutan roles: report mapping/precedence. Source `include` only local, confined same selected import root, depth≤4/files≤16, total≤1 MiB, cycle checked; never remote. `tokenColors`/`semanticTokenColors` preserved only as unsupported-field counts/report until syntax consumer; no .tmTheme loading or extension execution. Resolve include first, child overrides. uiTheme/type determines polarity; require variant if package contributes several themes. |
| Zed family JSON | Pick exactly one themes[] entry. style.background/text/text.muted/text.placeholder/border/element.background/element.hover/surface.background/panel.background→ui.background/foreground/text.muted/text.faint/border/surface/surface.hover/surface/sidebar-right.background (element.background wins for surface); text.accent→ui.accent; success/warning/error→state roles; created/deleted/modified and their backgrounds→diff; terminal.background/foreground and the 16 terminal.ansi color-name properties→terminal keys, changing source `bright_black` etc. to target `bright-black`; players[0].cursor/selection **not** terminal colors | Use actual schema properties, not the high-level guide's shorthand `foreground`/`accent`; inspect schema version and document mappings. Zed themes don't necessarily define terminal cursor/selection; leave Hutan defaults and report. terminal.ansi.background/dim_* and terminal.bright_foreground/dim_foreground, syntax/players/accents/windows/vibrancy are not imported; no Zed Rust crates or extensions activated. |
| WezTerm standalone TOML | colors.foreground/background/cursor_bg/cursor_fg/selection_bg/selection_fg, ansi[8], brights[8] → terminal keys | No `.wezterm.lua`; cursor_border separate from cursor_bg cannot map losslessly (prefer cursor_bg, warn if different); indexed/compose/tab-bar/split roles report unsupported. |
| Warp YAML | name/details→metadata/polarity; accent→ui.accent; cursor→terminal.cursor (accent if omitted); background/foreground; terminal_colors.normal/bright named colors→ANSI | Safe YAML parser with aliases/tags disabled, byte/depth bounds; gradients and background_image ignored with explicit warnings, never opened/fetched. Do not flatten a gradient to a misleading arbitrary color. |

Export initially means **lossless native TOML + SVG pack**; foreign-format export is not TH-1–4 acceptance. When added, terminal-only Ghostty/Alacritty/iTerm exports must report dropped chrome/fonts/icons and cannot be called a lossless theme export. Licensing/provenance/notices survive native export. No direct cross-product icon imports: a semantic-id mapping needs intentional human curation, and VS Code product themes use fonts outside Hutan's safe subset.

### 5.4 Optional later distribution

Do **not** block local authoring on a registry or installer. A later `hutan theme install https://host/owner/repo --ref <commit> [--subdir <pack>]` may stage a bounded snapshot, inspect license and pack content, show preview, and atomically install with explicit confirmation; selection remains separate. Require HTTPS, pinned commit/content hash, no hooks/submodules/LFS filters/symlinks, no arbitrary git/ssh/file/ext protocols or credential helpers spawned during untrusted discovery; no auto-update execution. A straightforward HTTPS archive/snapshot downloader can be safer than unrestricted `git clone`. Updates must preserve local edits by refusing overwrite or installing a new version; no silent nightly changes. This is optional future distribution work, not an unfinished requirement of TH-1–4.

## 6. Implementer-ready slices

Shared precondition: accept ADR for TOML schema/persistence/desktop-only/native-menu exception; use this document plus Chrome V3's canonical icon catalog as the contract. Dependency direction remains UI→settings with no GPUI in settings. Main integrator runs gates once after integration; tests below are requirements for implementers/main, **not checks run for this spike**.

### TH-1 — Native color pack loader, dogfooded built-ins, complete visual cutover

**Scope/files:** `crates/settings/src/themes.rs` → a `themes/` module (manifest/token registry/resolver/catalog/validation; keep only useful public API), `appearance.rs`, `store.rs`/`watch.rs`/`error.rs`; new `assets/themes/hutan-dark/theme.toml`, `hutan-light/theme.toml`; `crates/desktop/src/theme.rs`, `settings.rs`, `workbench/style.rs`, `view.rs`, `terminals.rs`, `ui.rs`, `sidebars.rs`, `chat/{markdown_view,cards}.rs`, state/diff consumers; `crates/terminal_view/src/{theme,settings,element}.rs`. New proposed files are implementation scope, not existing-file evidence. Ship API/schema documentation by updating existing `docs/design.md` and architecture/settings docs as appropriate.

**Contract:** GPUI-free declarations and typed resolved colors/fonts/metrics; app registry generation; existing terminal OSC path unchanged; selection/overrides in appearance; no project pack auto-load; sparse extends and explicit complete root mode. Every current palette literal/table/font/density/effect role identified above moves into embedded TOML or a documented semantic fallback; no competing hard-coded palettes remain in terminal_view. Preserve visual defaults (document alpha quantization and status-height reconciliation). Terminal selection fg/cursor fg added end-to-end. No renderer maps/string lookups or I/O per frame; only immutable snapshots. New file watch bounds and last-good behavior are part of this slice, not postponed to UI.

**Tests:** parse root/sparse manifests; all known tokens covered and unique; dark/light golden snapshots; implicit state/diff fallbacks respond to core changes; malformed/unknown keys and unsupported schema; metadata/size/depth caps, duplicate ids and builtin shadow self-reference; alpha/opaque-role checks; independent terminal pinning; explicit user font precedence vs pack defaults; geometry bounds/coupled metrics; no-follow traversal/race fixtures; atomic-save/reload/overflow/last-good/removal/reappearance; indexed/truecolor/OSC/reset semantics unchanged; cursor glyph and selection fg; no GPUI dependency in settings; terminal reports updated defaults once per committed style generation.

**Acceptance:** editing a user theme changes live chrome/chat/diff/terminal/fonts/density without restart; invalid edit leaves last-good frame and diagnostics; changing mode picks independently configured polarity; modifying an inactive pack doesn't invalidate terminal rows; default startup works with an empty/missing themes folder; existing `terminal.theme` ids still work without old resolver duplication. Native visual review in light/dark and both densities, including cursor/selection and modal contrast, is required by the integrator.

### TH-2 — Icon pack registry, safe SVG pipeline, Phosphor semantic cutover

**Depends:** TH-1 catalog/watcher/diagnostic primitives + ChromeV3Design exhaustive icon catalog. **Scope/files:** `crates/settings/src/themes/` icon manifest/safety module; `assets/icon-themes/phosphor/` root manifest+only used SVGs+MIT notice; `crates/desktop/src/icons.rs`, `workbench/registry.rs`, panel registration and all decorative-glyph/custom-drawn dock categories in §1.6; GPUI asset install wiring. Keep daemon/native PNG assets untouched.

**Contract:** typed semantic ids, same flat TOML, currentColor monochrome sanitizer, default-per-id, bounded shared asset source with content-addressed reload keys. All listed product symbols migrate, not just sidebar SVGs; retain terminal/user text/diff punctuation/keyboard symbols. Native menu-bar exemption explicitly documented in author catalog; no unsupported `brand.menu-bar` promise. Root Phosphor pack exhaustive over registered ids and exact catalog choices; default license recorded. Remove obsolete Lucide enum/file-path coupling, old glyph fallback paths and unused SVGs after their callers migrate, without deleting user assets.

**Tests:** registered-id coverage; default asset safety/serving/currentColor and license presence; omit one id→Phosphor for that id; multiple ids→same file; unknown id warning; dark/light matching; reload changes actual SVG cache key/bytes, not just registry row; bounded retired generations; malicious SVG scripts/events/entities/external URLs/CSS/data images/text/fonts/filter/use/huge path cases; traversal/symlink/nonregular files; renderer fixture proves themed close/search/dock control and unchanged click/keyboard behavior/accessibility names.

**Acceptance:** installing the two-icon example changes exactly those semantic controls; every other icon stays available; live SVG save updates visible icon; malformed asset warns and preserves last-good/default, never crashes or fetches; switching icon theme does not change colors. V3 Dashboard/dock work must call this typed id interface, not re-embed Phosphor files directly. Review no residual decorative glyphs in the identified categories; text exceptions recorded rather than blanket substitution.

### TH-3 — Settings pickers, reversible preview, diagnostics and local CLI

**Depends:** TH-1/TH-2. **Scope/files:** `crates/desktop/src/workbench/settings_{sheet,schema,pages,rows,layers,labels}.rs`, new appearance picker module if needed, actions/preview style plumbing; `crates/cli/src/args.rs`, `commands.rs` and new `commands/theme.rs`; settings shared validator/export/scaffold helpers. Coordinate Settings V2 research rather than create a second diagnostics/picker framework.

**Contract:** §5.1/5.2 `ls|validate|new|export`, independent appearance/icon pickers, explicit commit/cancel, Problems footer; atomically writes only local user packs/settings. Export root=true completeness rule is public and tested, no private serialization. CLI commands run with absent daemon. Preview cannot leak through app-global style to another window; committed selection propagates consistently.

**Tests:** picker catalog reflects add/delete/invalid files; Enter persists once, Escape/click-outside/settings-close cancels without write; system mode and external settings changes during preview restore latest effective state; independent icon/color picks; diagnostics-only generation updates Problems; invalid selection fallback; user overrides and reset remove keys rather than write redundant defaults; two-window preview isolation; export/reimport resolution equivalence and notice/provenance preservation; CLI `--json`, exit statuses, kind disambiguation/no-clobber/temp HUTAN_HOME, `new`→validate→select complete flow; no daemon startup/socket access.

**Acceptance:** a user creates and validates a theme, edits it, previews/cancels/applies in Settings and exports it with no rebuild; erroneous file/key shows one actionable Settings/CLI diagnostic, not a crash or silent fallback. Icon example works through the same authoring flow. Existing settings/keymap/layer diagnostics remain intact.

### TH-4 — Safe, explicit color importers

**Depends:** TH-1 and TH-3 CLI/preview/export contract. **Scope/files:** proposed `crates/settings/src/themes/import/{ghostty,iterm2,alacritty,vscode,zed,wezterm,warp}.rs`, CLI theme command, Settings import affordance using the same result type; format fixtures under settings tests. No peer code dependencies; review added XML/plist/JSONC/YAML parsing crates under the repo's license policy. Native packs remain TOML only.

**Contract:** every row of §5.3, variant selection, allowlist mapping, loss report, source metadata/license policy, dry-run, no auto-activation or network. Syntax-specific fields explicitly unsupported until M11; importer does not require creating an editor/highlighter. VS Code include confinement/cycles and inert plist DOCTYPE are explicit exceptions to native packs' no-includes/no-DTD declarations.

**Tests:** official-format minimal/representative fixtures for each format; exact ANSI ordering/duplicate palette semantics; float→byte conversion/color-space error; JSONC comments/trailing comma handling; Zed family choice; known-field mapping precedence; missing fields inherit; unknown license marker; malformed/capped/growing files; include cycles/remote/traversal and malicious XML/YAML tags/aliases; arbitrary Ghostty shell/font/env config never applied; no Lua or VSIX execution; `--dry-run` leaves temp tree unchanged; import→native export→parse yields equal resolved supported roles and a stable loss report.

**Acceptance:** for each promised format, import a local color scheme through CLI and Settings, preview before save, select generated native pack; supported colors match source values and unsupported fields are visible. Importing never changes keymaps/shell/env/permissions, touches other app config, starts daemon or performs network requests. Native export remains lossless; don't silently advertise foreign export or icon conversion.

## 7. User decisions (five maximum)

These are recommendations to ratify, not reasons to block preparing briefs:

1. **Format and packaging:** choose native TOML folder packs plus single-file color shorthand; schema v1, explicit built-in inheritance and complete root mode. Recommend **yes**: matches settings.toml/keymap.toml and is self-contained/offline.
2. **Externalization breadth:** include chrome+terminal+state/diff+fonts/density/effects, independently chosen product icons; no arbitrary layout rules, code or embedded fonts. Recommend **yes**, with user font/zoom overrides winning and syntax explicitly deferred to its real consumer, not faked.
3. **Trust/project scope:** user directory only now, no automatic repository packs; explicit import/preview of project-authored files. Recommend **yes** until window-local project trust exists; terminal.theme keeps its existing scope.
4. **Default icon identity/native exception:** Phosphor per Chrome V3's canonical map, sparse per-id fallbacks; keep macOS template menu-bar/app/notification identity fixed in both desktop and daemon. Recommend **yes** to preserve “desktop-only, daemon untouched”; if menu-bar customization is a hard requirement, approve a separate native branding contract rather than silently expand TH-2.
5. **Interoperability/distribution:** implement the seven explicit local color importers with truthful loss reports after native authoring; native export first; no installer/marketplace in TH-1–4. Recommend **yes**; add HTTPS pinned git-snapshot installation later only if sharing friction warrants it.

## Evidence and verification boundary

This spike established source-level inventory and official format/license documentation only. It did **not** verify rendered appearance, accessibility compliance, GPUI SVG sanitizer compatibility, live reload behavior of a new loader, importer conversions or any CLI proposed above. Those are acceptance gates for the slices, with exact test scenarios specified, not implied successes. The implementer briefs must use the final Chrome V3 icon catalog as the authoritative id→Phosphor map and run the main repository gate/native demos only in the assigned implementation workflow.
