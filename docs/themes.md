# Themes

Hutan's look is data. A **color pack** sets chrome, terminal palette, state/diff colors, fonts, density and effects. An independent **icon pack** maps stable semantic ids to monochrome SVG artwork. Built-ins (`hutan-dark`, `hutan-light`, `phosphor`) are embedded manifests parsed by the same loaders as user packs. Decision: ADR 0018.

A pack is data only: no code, no CSS, no network, no font files. The daemon never sees a pack; only the desktop reads them.

## Where packs live

```text
<data dir>/                    # $HUTAN_HOME, else ~/Library/Application Support/Hutan
  settings.toml
  themes/
    canopy-night/
      theme.toml               # one folder per pack
      LICENSE                  # optional; named by license_file
    paper.toml                 # or a single file, no assets
```

Only this folder is read: no repository or project packs are loaded (a repository must not restyle approval surfaces by being opened), no archives, no search paths, no includes. Packs are found exactly one level deep, in sorted order, at most 256 entries. Symlinks anywhere below `themes/` are refused. Without a `themes/` folder, Hutan runs on the two built-ins.

## A pack

A sparse pack extends a built-in and sets only what it changes:

```toml
schema_version = 1
id = "canopy-night"            # lower-kebab, 1-64 chars, not `auto`; independent of the file name
name = "Canopy Night"
authors = ["Someone"]
license = "MIT OR Apache-2.0"  # SPDX expression or LicenseRef-*
appearance = "dark"            # dark | light: which slot it can fill
extends = "builtin:hutan-dark" # builtin:hutan-dark | builtin:hutan-light, same appearance
# optional: version, description, repository (https), license_file

[colors]
"ui.accent" = "#d99a58"
"ui.on-accent" = "#1f1e1d"
"terminal.background" = "#101820"
"terminal.selection.foreground" = "#ffffff"

[fonts]
mono = ["JetBrains Mono", "Menlo"]

[density.compact]
"sidebar.row-height" = 28

[metrics]
"terminal.font-size" = 14
```

A **root** pack (`root = true`, no `extends`) must declare every required color, both density tables, every metric and every font role. The built-ins are root packs; copy one to start from a complete example.

- Colors are `#rrggbb` or `#rrggbbaa` (either case, straight alpha, sRGB). Keys are quoted dotted ids (`"ui.border"`); nested tables under `[colors]` are an error. Text and terminal colors must be opaque; surfaces, borders, effects and tints may be translucent.
- Unknown keys and tokens warn and are ignored (`syntax.*` and `editor.*` are real tokens: see [Editor and syntax colors](#editor-and-syntax-colors)). A wrong type, a malformed color, a number out of range or an invalid `extends` is an error and the pack is not loaded.
- `schema_version` must be `1`; any other value disables the pack with a diagnostic.
- Numbers are never clamped: a value outside its range is an error. Heights must leave room for their text (height >= text size + 2), `nav.line-height >= nav.text.size`, `strip.button.size >= nav.icon.size`, `titlebar.height >= strip.button.size`.
- `[fonts]` roles are ordered lists of installed families; the first installed one is used (the system UI font always is), the last one if none is. Hutan loads no font files.

### Resolution

1. The built-in named by `extends`; 2. the pack's declared values; 3. the user's overrides (below); 4. every optional color the pack leaves open is **derived** from the colors above it in the table, so changing `state.success` also recolors the default `diff.added`, `terminal.block.success`, ... The "If not set" column of the reference is that algorithm. Nothing is looked up by name at paint time: the desktop gets an immutable resolved snapshot.

Optional terminal roles reach the terminal view as "declared or not": `terminal.cursor.foreground` and the four `terminal.block.*` fills, when not declared, are derived from the terminal colors *in effect*, so an application that sets its own background (OSC 11) keeps matching tints. `terminal.selection.foreground`, when absent, keeps each cell's own text color.

## Choosing a pack

```toml
[appearance]
appearance = "system"          # system | light | dark: which slot is active (unchanged)
light_theme = "hutan-light"    # pack of light windows
dark_theme = "canopy-night"    # pack of dark windows

[appearance.theme_overrides."canopy-night".colors]
"ui.accent" = "#d99a58"        # laid over the pack, wins over it

[terminal]
theme = "auto"                 # auto | any pack id
```

- `light_theme` must name a light pack and `dark_theme` a dark one. A missing, invalid or wrong-polarity pack falls back to the built-in of that slot, with a Problem naming the setting; the request is kept, so installing or fixing the pack applies it again at once.
- `terminal.theme = "auto"` is the active pack's terminal section. A pack id picks only that pack's terminal colors and leaves the chrome alone (`hutan-light` in a dark window still works). An unknown id falls back to `auto` with a Problem.
- A bad override value (not `#rrggbb[aa]`, or an alpha on an opaque role) rejects the override table: the previous accepted overrides stay, or none apply if there were none. An unknown token is a warning. Overrides never change the pack file.
- Terminal font family, size and line height: the user's `terminal.font_family`, `font_size` and `line_height` win **only if written** in a settings layer (user, workspace or project) or set by the zoom keys; otherwise the pack's `[fonts] mono`, `terminal.font-size` and `terminal.line-height` apply. Density (`appearance.density`) is the user's; the pack supplies the numbers of each density.

## Picking a pack in Settings

*Settings ▸ Appearance* has a picker for the **light theme**, the **dark theme** and the **icon theme** (beside the mode and density); *Settings ▸ Terminal* has one for the **color theme** (Automatic, or any valid color pack: its terminal colors only). Each is the same control:

- **The list.** Every pack of the registry with its name, id, author(s), license, version and origin: *Embedded*, *User*, *User override* (your pack of a built-in's id) or *Imported* (its `[provenance]` names a `source_format`), plus *Last good version* for a pack whose latest edit is invalid. A light slot lists light packs and a dark slot dark ones; an icon pack that declares a polarity is tagged. A pack that cannot load is listed **disabled** with its errors (file, key, reason) and cannot be picked. Typing searches by name, id or author. The list follows the folders live.
- **Preview, without applying.** Moving through the list (↑ ↓ or the pointer) shows the pack under the cursor in the picker's own preview; nothing is written and nothing in the window changes. A color pack is shown as sample chrome (sidebar rows, card, buttons, link), state cards, a diff and a terminal row with its 16 ANSI colors, with the pack's warnings under it; an icon pack as a grid of navigation, action, state and dock icons, each marked *this pack* or *Phosphor* (the fallback), with one fallback called out. The preview is drawn from the pack's own values by the picker alone: it never touches the window's style, the installed icons or another window, so there is nothing to restore.
- **Apply and cancel.** **Return** (or **Apply**, or a click on a pack) writes the selection once. Choosing what is already in effect writes nothing, and choosing the embedded default removes the key instead of writing it. **Escape**, a click outside the picker, closing Settings and quitting cancel: the window shows the current persisted state, a settings-file or system-mode change made while the preview was open included.
- **Actions** (under each setting). **Open pack folder** (`themes/` or `icon-themes/`, created if missing), **Validate / Reload** (reads the folders again and says how many packs and problems there are), **Duplicate as new pack** (a copy of the highlighted pack under a free id, `<id>-copy`, ready to edit; it selects nothing), **Reset to embedded default** (removes the key).
- **A theme nobody can read** never removes the way back: *View ▸ Reset Theme and Icons to Default* (a native menu), the palette command of the same name and the *Reset to embedded default* row of the picker each remove `appearance.light_theme`, `dark_theme`, `icon_theme`, `theme_overrides` and `terminal.theme` from your settings file.

## The `hutan theme` command

Local only: it reads and writes the data folder (`$HUTAN_HOME`, else `~/Library/Application Support/Hutan`) and never starts or contacts the daemon. `--kind color|icon` (default `color`) picks the kind; `--json` prints one versioned document (`"version": 1`).

```text
hutan theme ls [--kind color|icon] [--json]
hutan theme validate <id-or-path> [--kind color|icon] [--strict] [--json]
hutan theme new <id> --name <name> --author <author> --license <spdx>
                [--extends builtin:hutan-dark] [--kind color|icon] [--output <dir>]
hutan theme export <id> --output <dir> [--kind color|icon] [--resolved]
```

- **`ls`** lists id, name, appearance, origin, status (`ok`, `ok (1 warning)`, `invalid (2 errors)`) and where the pack is selected (`light`, `dark`, `icon`), then the problems of the folder.
- **`validate`** judges a pack exactly as the desktop does: manifest, inherited values, assets, contrast and license. The target is an installed id (embedded ones included) or a path: anything with a `/`, starting with `.` or `~`, or ending in `.toml` is a path (`./name` for a folder named like an id). A folder of the other kind is named: `holds an icon pack; use --kind icon`.
- **`new`** writes `<output>/<id>/` (default: `themes/` or `icon-themes/` of the data folder): a complete **sparse** pack with an explicit `extends`, your metadata, a version and one override (the base's accent, for you to change); an icon pack also gets `icons/close.svg`, a safe example mapped to `action.close`. The manifest is checked by the loader before anything is written, and nothing is ever replaced: an existing `<id>` folder is an error.
- **`export`** writes the installed pack as an editable native pack into `<output>/<id>/` (which must not exist): the manifest with its comments and `[provenance]`, the license file, and for an icon pack every referenced SVG. An embedded pack exports as its embedded source. **`--resolved`** writes a self-contained `root = true` pack instead: every required token, both density tables, every metric and font role (a color pack), or all icons with the embedded Phosphor artwork and its notices (`LICENSE-phosphor`, `PROVENANCE-phosphor`; an icon pack's `license` then reads `(<license>) AND MIT`). Optional colors that were derived stay derived, `[provenance]` is kept and `exported_from` added, and for a color pack your `appearance.theme_overrides` of that pack are laid over it, so the file is what Settings shows. Reading an export back gives an equal resolved theme.

Exit status: **0** ok (warnings included), **1** errors in a pack, a pack that is not found, or a file that cannot be read or written (`validate --strict` also for warnings), **2** an invalid invocation (a missing option, an id that is not a pack id, an unknown base). `ls` exits 0 when it could read the folders, even if some packs are invalid (they are listed with their errors). Importers (`hutan theme import`) arrive with TH-4.

```sh
hutan theme new canopy-night --name "Canopy Night" --author "Me" --license "MIT OR Apache-2.0"
$EDITOR "$HOME/Library/Application Support/Hutan/themes/canopy-night/theme.toml"
hutan theme validate canopy-night --strict          # same lines as Settings > Problems
# pick it in Settings > Appearance (↑ ↓ preview, Return applies), then share a flat copy:
hutan theme export canopy-night --resolved --output ~/Desktop
hutan theme new canopy-lines --kind icon --name "Canopy Lines" --author "Me" --license MIT
```

## Editor and syntax colors

The code editor reads two families of tokens from the same pack, both optional: a pack that sets none of them still has a complete editor.

- **`syntax.*`**: one opaque text color per token of the editor's highlighter, with exactly the names of `hutan_syntax::Token` (`syntax.comment`, `syntax.string`, `syntax.string.escape`, `syntax.number`, `syntax.constant`, `syntax.keyword`, `syntax.operator`, `syntax.function`, `syntax.function.method`, `syntax.type`, `syntax.type.builtin`, `syntax.variable`, `syntax.variable.parameter`, `syntax.property`, `syntax.attribute`, `syntax.tag`, `syntax.punctuation`, `syntax.punctuation.bracket`, `syntax.markup.heading`, `syntax.markup.bold`, `syntax.markup.italic`, `syntax.markup.link`). A test keeps the two lists equal; which grammar capture becomes which token is the syntax crate's table (`Token::from_capture`), not the pack's. A name below `syntax.` that is not one of these is an unknown token (a warning).
- **`editor.*`**: the surface: `editor.background`, `editor.foreground`, `editor.gutter.background` / `.foreground`, `editor.line-number.active`, `editor.current-line`, `editor.selection` / `.selection.inactive`, `editor.cursor`, `editor.whitespace`, `editor.indent-guide`, `editor.bracket-match`, `editor.search-match` / `.search-match.active`, `editor.diagnostic.error|warning|info|hint` (the colors of the underlines), `editor.diff-gutter.added|modified|removed` and `editor.agent-changed` (the highlight of lines an agent changed). Tints (`current-line`, `selection`, `search-match`, ...) may be translucent and are drawn over the editor background; text, cursor and underline colors are opaque.

**Derivation.** Every one of them has a fallback in the reference below, so a sparse pack gets a complete editor from the core colors it already sets: `editor.background` and `editor.foreground` follow `ui.background` and `ui.foreground`; the diff gutter follows `diff.*` (so `state.success` recolors it), the diagnostic underlines follow `state.error`, `state.warning`, `ui.accent` (info) and `ui.text.muted` (hint); selection, bracket match and agent-changed are tints of `ui.accent`, search matches of `state.warning`. Without `syntax.*` colors the highlighter uses the terminal palette: strings green, escapes and types cyan, numbers, constants and attributes yellow, keywords magenta, functions blue, tags red; comments, punctuation and brackets are `ui.text.muted`; variables, parameters, properties and operators are the editor foreground. Only the normal ANSI colors (0 to 7) are used, which the terminal contrast check already holds to 4.5:1. Both built-in packs declare all 22 `syntax.*` colors (below); a sparse pack that extends one inherits them, and changing `terminal.ansi.green` then does not move `syntax.string`: write the token to change it. `editor.*` is derived in both built-ins. `ResolvedTheme::editor()` gives the editor view one immutable `EditorTheme`: the surface roles and the 22 syntax colors in the order of `hutan_syntax::Token::ALL` (index with `token as usize`), so a theme change restyles without re-parsing.

**Contrast.** `syntax.*` and `editor.foreground` on `editor.background` join the contrast advice (text, 4.5:1; Problems key `contrast`, warning only, a pack is reported for what it makes worse than its base). Both built-ins meet it for every token, and a test fails if one stops:

<!-- syntax-contrast:begin -->

| Token (`syntax.` prefix) | hutan-dark | Ratio | hutan-light | Ratio |
|---|---|---|---|---|
| default text (`editor.foreground`) | `#f5f4ef` | 13.77:1 | `#1f1e1d` | 15.80:1 |
| `comment` | `#94928a` | 4.86:1 | `#6f6d66` | 4.92:1 |
| `string` | `#86d19c` | 8.38:1 | `#2f7a48` | 4.99:1 |
| `string.escape` | `#5fc2c6` | 7.23:1 | `#167780` | 5.00:1 |
| `number` | `#f2a27a` | 7.38:1 | `#a8480f` | 5.54:1 |
| `constant` | `#f5b48a` | 8.50:1 | `#9a4a1a` | 5.92:1 |
| `keyword` | `#daaee6` | 8.07:1 | `#8e4ea8` | 5.24:1 |
| `operator` | `#c8c6be` | 8.86:1 | `#4a4944` | 8.56:1 |
| `function` | `#8fb9f2` | 7.50:1 | `#2b62b8` | 5.63:1 |
| `function.method` | `#a6c8f5` | 8.81:1 | `#1d4f99` | 7.58:1 |
| `type` | `#eec261` | 9.04:1 | `#8a6300` | 5.16:1 |
| `type.builtin` | `#e2ae45` | 7.49:1 | `#7a5600` | 6.31:1 |
| `variable` | `#eae8e1` | 12.37:1 | `#1f1e1d` | 15.80:1 |
| `variable.parameter` | `#d9c7a3` | 9.13:1 | `#5a4a2a` | 8.14:1 |
| `property` | `#7dd3d6` | 8.78:1 | `#1a6f8f` | 5.37:1 |
| `attribute` | `#c996d8` | 6.38:1 | `#a0402a` | 6.11:1 |
| `tag` | `#f08c7a` | 6.31:1 | `#b93a26` | 5.39:1 |
| `punctuation` | `#a6a49c` | 6.07:1 | `#6f6d66` | 4.92:1 |
| `punctuation.bracket` | `#bdbbb3` | 7.89:1 | `#57554f` | 7.07:1 |
| `markup.heading` | `#f2a27a` | 7.38:1 | `#a8480f` | 5.54:1 |
| `markup.bold` | `#f5f4ef` | 13.77:1 | `#1f1e1d` | 15.80:1 |
| `markup.italic` | `#d6d4cc` | 10.21:1 | `#4a4944` | 8.56:1 |
| `markup.link` | `#8fb9f2` | 7.50:1 | `#2b62b8` | 5.63:1 |

On `editor.background`: `#262624` (hutan-dark) and `#faf9f5` (hutan-light).

<!-- syntax-contrast:end -->

## Hot reload

One watcher per application watches `themes/` (creation, removal, single-file packs) and each pack folder (edits in place; atomic-save rename works). Events go through a bounded queue and a debounce (100 ms quiet, 500 ms at most) into one worker that rescans; a flood coalesces into one rescan. Editing the active pack restyles chrome, chat, diffs, terminal rows, fonts and density without a restart, and the daemon learns the new terminal colors once. An edit that changes nothing publishes nothing, and editing a pack that is not in use changes only the catalog (pickers, Problems): no view is invalidated.

- An invalid edit of a pack that was valid keeps the **last good** version and adds Problems (`kept the previous version of theme ...`). A pack that was never valid is listed disabled and cannot be selected. A removed pack disappears; one that reappears loads again.
- If the watcher cannot start, hot reload is off: a Problem says so and **Validate / Reload** (below) is the way to read the folders again.
- A window that is fully covered by other windows shows its last frame: GPUI stops presenting frames while a window is occluded and repaints as soon as it is visible again (≈0.5 s, no input needed). Visible windows, including ones side by side with your editor, update in under a second (measured 0.16–0.89 s, `docs/spikes/2026-10-21-ux-audit.md` §4).

## Limits and safety

| Limit | Value |
|---|---|
| Manifest size | 256 KiB (checked on the real read length too); 16 MiB of manifests per scan |
| Candidate entries per scan | 256 (the rest are reported, not read) |
| Color, density and metric entries | at most 512 per manifest |
| Table nesting | 5 levels |
| Diagnostics kept | 512 plus one summary line |
| Font families per role | 8, each at most 120 characters |

Paths (`license_file` and icon assets) are normalized relative UTF-8 paths with no `..`, absolute, `~`, `:`, backslash or empty component. Descriptor-relative `openat(O_NOFOLLOW)` opens every component, the final descriptor must be a regular file, and identity checks reject replacements during the read. A symlink swap cannot redirect the read outside the configured data root.

## Problems and contrast

Pack and selection problems appear in the Settings Problems list (the same list as settings errors), one line each, with the file, the key (`colors.ui.border`, `icons.action.close`, `appearance.dark_theme`) and the reason, for color and icon packs alike (`hutan theme validate` prints the same lines). A change that only adds, fixes or removes a problem (an unused pack, a bad icon file) updates the list without restyling anything. Contrast is advice only (key `contrast`, WCAG 2.2 ratios, translucent colors composed over what they are drawn on): a user pack is reported for the pairs it makes worse than its base (a root pack is judged against the built-in of its polarity, so an exported flat copy reports what its source did). The built-ins' own known shortfalls (today only the glyph under the block cursor of `hutan-light`) are a reviewed baseline in `crates/settings/tests/it/themes.rs`; they are reported nowhere else and nothing is repaired. A `license` that is not an SPDX expression (`MIT OR Apache-2.0`) or a `LicenseRef-<name>` marker, or is `LicenseRef-Unknown`, is a warning (key `license`; a syntax check, not legal advice).

The pairs checked are the chrome text on its surfaces, the terminal palette on the terminal background and the editor: `editor.foreground` and every `syntax.*` color on `editor.background` (4.5:1).

The chrome pairs are the workbench's (design direction `docs/spikes/2026-10-21-clean-aesthetic.md` §3.2.3), and a unit test in `crates/settings/src/themes/contrast.rs` fails when `hutan-dark` or `hutan-light` stops passing one (`cargo nextest run -p hutan-settings --no-capture -E 'test(workbench_pairs)'` prints each pair with its ratio):

| Pair | Needs |
|---|---|
| `ui.foreground`, `ui.text.muted` on `ui.background`, both sidebars, `ui.surface`, `ui.surface.hover`, both selected rows, `ui.accent.soft` | 4.5:1 |
| `ui.on-accent` on `ui.accent` (dark ink on the brand orange) | 4.5:1 |
| `state.success`, `state.warning`, `state.error` as words on `ui.background`, `ui.surface`, `ui.sidebar.background`, `ui.surface.hover` | 4.5:1 |
| the same three as glyphs on `ui.sidebar.selected` (the word there is `ui.foreground`) | 3:1 |
| `markdown.link` on `ui.background`, `ui.surface`, `ui.sidebar.background` (links are the only accent text) | 4.5:1 |
| `ui.accent` as a fill on `ui.background` and `ui.surface` | 3:1 |

`ui.text.faint` is disabled text only and is not checked; placeholders are `ui.text.muted`. `ui.focus` is the optional keyboard focus ring color (2 px, drawn only while focus came from the keyboard); a pack that leaves it out gets the macOS keyboard focus indicator color, which the desktop reads at runtime.

## Differences from the hard-coded look

The built-in packs reproduce the previous colors, sizes and fonts. The only deliberate differences are 8-bit alpha quantization of values that were fractions: the dark hairline `#ffffff12` is alpha 0.0706 (was 0.07), the shadow alphas are 0.1216 (light) and 0.4 (dark) (were 0.12, 0.40), and the scrim 0.2784 and 0.5020 (were 0.28, 0.50). Derived tints keep the exact old fractions (0.08, 0.10, 0.16, 0.22, 0.28, 0.13, 0.07). The status bar is 24 px in both densities, as drawn; the unused 22 / 26 table is gone.

Titlebar height, nav row and strip sizes (`titlebar.height`, `nav.*`, `strip.*`) are loaded, validated and resolved, but the layout code that draws them (`render.rs`, `sidebars.rs`) still reads constants equal to the built-in values (a test keeps `titlebar.height` and the constant equal); they take effect once the dock work that owns those files lands.

## Token reference

Generated from the token registry (`crates/settings/src/themes/tokens.rs`) by `UPDATE_GOLDEN=1 cargo nextest run -p hutan-settings --test it -E 'test(/^themes::/)'`; a test fails if this section drifts.

<!-- tokens:begin -->

#### Colors

| Token | Alpha | If not set | Used for |
|---|---|---|---|
| `ui.background` | may be translucent | required in a root pack | Window and center panes. |
| `ui.sidebar.background` | may be translucent | required in a root pack | Left dock and the title bar strip. |
| `ui.sidebar.selected` | may be translucent | required in a root pack | Selected nav row and active project card of the left sidebar. |
| `ui.sidebar-right.background` | may be translucent | required in a root pack | The right sidebar surface. |
| `ui.sidebar-right.selected` | may be translucent | required in a root pack | Hover and selected rows of the right sidebar. |
| `ui.surface` | may be translucent | required in a root pack | Cards, composer, popovers, palette and modals. |
| `ui.surface.hover` | may be translucent | required in a root pack | Row and button hover fill; inline code background in prose. |
| `ui.border` | may be translucent | required in a root pack | Hairlines (1 px). Usually translucent in dark packs. |
| `ui.foreground` | opaque | required in a root pack | Primary text. |
| `ui.text.muted` | opaque | required in a root pack | Secondary text and icons. |
| `ui.text.faint` | opaque | required in a root pack | Disabled text. |
| `ui.accent` | opaque | required in a root pack | Primary buttons, selection, focus, links: the brand color. |
| `ui.accent.soft` | may be translucent | required in a root pack | Selected row and chips. |
| `ui.on-accent` | opaque | required in a root pack | Text on an accent fill (primary buttons, the send button). |
| `state.success` | opaque | required in a root pack | Success: completed work, added lines, a connected daemon. |
| `state.warning` | opaque | required in a root pack | Warning: blocked agents, pending approvals, renamed files. |
| `state.error` | opaque | required in a root pack | Error: failed sessions, removed lines, a stopped daemon. |
| `ui.shadow` | may be translucent | required in a root pack | Color of the one soft shadow of popovers, the palette and modals. |
| `ui.scrim` | may be translucent | required in a root pack | Dimming layer behind modals and sheets. |
| `terminal.foreground` | opaque | required in a root pack | Default text color of terminals. |
| `terminal.background` | opaque | required in a root pack | Default background of terminals. |
| `terminal.cursor` | opaque | required in a root pack | Cursor color. |
| `terminal.selection.background` | opaque | required in a root pack | Background of selected cells. |
| `terminal.ansi.black` | opaque | required in a root pack | ANSI color 0. |
| `terminal.ansi.red` | opaque | required in a root pack | ANSI color 1. |
| `terminal.ansi.green` | opaque | required in a root pack | ANSI color 2. |
| `terminal.ansi.yellow` | opaque | required in a root pack | ANSI color 3. |
| `terminal.ansi.blue` | opaque | required in a root pack | ANSI color 4. |
| `terminal.ansi.magenta` | opaque | required in a root pack | ANSI color 5. |
| `terminal.ansi.cyan` | opaque | required in a root pack | ANSI color 6. |
| `terminal.ansi.white` | opaque | required in a root pack | ANSI color 7. |
| `terminal.ansi.bright-black` | opaque | required in a root pack | ANSI color 8. |
| `terminal.ansi.bright-red` | opaque | required in a root pack | ANSI color 9. |
| `terminal.ansi.bright-green` | opaque | required in a root pack | ANSI color 10. |
| `terminal.ansi.bright-yellow` | opaque | required in a root pack | ANSI color 11. |
| `terminal.ansi.bright-blue` | opaque | required in a root pack | ANSI color 12. |
| `terminal.ansi.bright-magenta` | opaque | required in a root pack | ANSI color 13. |
| `terminal.ansi.bright-cyan` | opaque | required in a root pack | ANSI color 14. |
| `terminal.ansi.bright-white` | opaque | required in a root pack | ANSI color 15. |
| `ui.sidebar.hover` | may be translucent | `ui.foreground` at alpha 0.08 | Hover fill of the left nav rows. |
| `ui.drop-target` | may be translucent | `ui.accent` at alpha 0.28 (dark) / 0.22 (light) | Drag-and-drop target highlight. |
| `ui.focus` | opaque | none: not drawn | The 2 px keyboard focus ring, drawn only while focus came from the keyboard; absent: the macOS keyboard focus indicator color, read by the desktop at runtime. |
| `state.working` | opaque | `ui.accent` | A working or starting session. |
| `state.starting` | opaque | `ui.accent` | A session that is starting. |
| `state.idle` | opaque | `ui.text.muted` | An idle session. |
| `state.unknown` | opaque | `ui.text.muted` | A session or agent in an unknown state. |
| `state.interrupted` | opaque | `ui.text.muted` | An interrupted agent. |
| `state.disconnected` | opaque | `ui.text.muted` | A disconnected agent. |
| `state.blocked` | opaque | `state.warning` | A blocked agent. |
| `state.approval` | opaque | `state.warning` | A session waiting for an approval. |
| `state.question` | opaque | `state.warning` | A session waiting for an answer. |
| `state.needs-you` | opaque | `state.warning` | The aggregate needs-you indicator. |
| `state.completed` | opaque | `state.success` | A completed session or agent. |
| `state.failed` | opaque | `state.error` | A failed session or agent. |
| `diff.added` | opaque | `state.success` | Added files and lines: text. |
| `diff.removed` | opaque | `state.error` | Removed files and lines: text. |
| `diff.modified` | opaque | `ui.accent` | Modified files: text. |
| `diff.renamed` | opaque | `state.warning` | Renamed files: text. |
| `diff.added.background` | may be translucent | `diff.added` at alpha 0.16 | Row tint of added lines. |
| `diff.removed.background` | may be translucent | `diff.removed` at alpha 0.16 | Row tint of removed lines. |
| `diff.hunk.background` | may be translucent | `ui.accent` at alpha 0.1 | Row tint of hunk headers. |
| `diff.hunk.foreground` | opaque | `ui.text.muted` | Text of hunk headers. |
| `terminal.cursor.foreground` | opaque | `terminal.background` | The glyph under a block cursor. |
| `terminal.selection.foreground` | opaque | none: not drawn | Text color of selected cells; absent keeps each cell's own foreground. |
| `terminal.block.success` | opaque | `state.success` | Gutter bar and chip of a command that exited 0. |
| `terminal.block.error` | opaque | `state.error` | Gutter bar and chip of a command that failed. |
| `terminal.block.neutral` | opaque | `ui.text.muted` | Running commands and commands without an exit code. |
| `terminal.block.accent` | opaque | `ui.accent` | Accent of the highlighted block. |
| `terminal.block.separator` | may be translucent | `terminal.background` mixed 0.16 of the way to `terminal.foreground` | Line between two blocks. |
| `terminal.block.highlight` | may be translucent | `terminal.block.accent` at alpha 0.13 | Tint of the highlighted block. |
| `terminal.block.hover` | may be translucent | `terminal.block.neutral` at alpha 0.07 | Tint of the hovered block. |
| `terminal.block.folded` | may be translucent | `terminal.background` mixed 0.07 of the way to `terminal.foreground` | Fill of a folded block's summary row. |
| `markdown.link` | opaque | `ui.accent` | Links in chat messages. |
| `markdown.inline-code.background` | may be translucent | `ui.surface.hover` | Background of inline code in chat messages. |
| `editor.background` | may be translucent | `ui.background` | Background of the code editor. |
| `editor.foreground` | opaque | `ui.foreground` | Default text of the code editor: code that no syntax token colors. |
| `editor.gutter.background` | may be translucent | `editor.background` | Background of the line-number gutter. |
| `editor.gutter.foreground` | opaque | `editor.foreground` mixed 0.4 of the way to `editor.background` | Line numbers. |
| `editor.line-number.active` | opaque | `editor.foreground` | The line number of the cursor's line. |
| `editor.current-line` | may be translucent | `editor.foreground` at alpha 0.05 (dark) / 0.04 (light) | Tint of the cursor's line. |
| `editor.selection` | may be translucent | `ui.accent` at alpha 0.32 (dark) / 0.26 (light) | Background of selected text in the focused editor. |
| `editor.selection.inactive` | may be translucent | `editor.selection` at alpha 0.5 | Background of selected text in an editor that does not have focus. |
| `editor.cursor` | opaque | `ui.accent` | The text cursor. |
| `editor.whitespace` | may be translucent | `editor.foreground` at alpha 0.25 | Visible whitespace marks (spaces, tabs). |
| `editor.indent-guide` | may be translucent | `editor.foreground` at alpha 0.1 | Vertical indentation guides. |
| `editor.bracket-match` | may be translucent | `ui.accent` at alpha 0.28 | Background of a bracket and its partner. |
| `editor.search-match` | may be translucent | `state.warning` at alpha 0.28 | Background of every find match. |
| `editor.search-match.active` | may be translucent | `state.warning` at alpha 0.6 | Background of the current find match. |
| `editor.diagnostic.error` | opaque | `state.error` | Underline of an error diagnostic. |
| `editor.diagnostic.warning` | opaque | `state.warning` | Underline of a warning diagnostic. |
| `editor.diagnostic.info` | opaque | `ui.accent` | Underline of an information diagnostic. |
| `editor.diagnostic.hint` | opaque | `ui.text.muted` | Underline of a hint diagnostic. |
| `editor.diff-gutter.added` | opaque | `diff.added` | Gutter bar of lines added since the last commit. |
| `editor.diff-gutter.modified` | opaque | `diff.modified` | Gutter bar of lines modified since the last commit. |
| `editor.diff-gutter.removed` | opaque | `diff.removed` | Gutter mark where lines were removed since the last commit. |
| `editor.agent-changed` | may be translucent | `ui.accent` at alpha 0.2 (dark) / 0.16 (light) | Background of lines an agent changed. |
| `syntax.comment` | opaque | `ui.text.muted` | Comments. |
| `syntax.string` | opaque | `terminal.ansi.green` | Strings, character literals, raw text and dates. |
| `syntax.string.escape` | opaque | `terminal.ansi.cyan` | Escape sequences inside strings. |
| `syntax.number` | opaque | `terminal.ansi.yellow` | Numbers. |
| `syntax.constant` | opaque | `terminal.ansi.yellow` | Constants: `true`, `null`, enum-like names. |
| `syntax.keyword` | opaque | `terminal.ansi.magenta` | Keywords, `self` and `this`. |
| `syntax.operator` | opaque | `editor.foreground` | Operators. |
| `syntax.function` | opaque | `terminal.ansi.blue` | Functions and macros: definitions and calls. |
| `syntax.function.method` | opaque | `syntax.function` | Methods. |
| `syntax.type` | opaque | `terminal.ansi.cyan` | Types, constructors, namespaces and lifetimes. |
| `syntax.type.builtin` | opaque | `syntax.type` | Built-in types. |
| `syntax.variable` | opaque | `editor.foreground` | Variables. |
| `syntax.variable.parameter` | opaque | `syntax.variable` | Function parameters. |
| `syntax.property` | opaque | `syntax.variable` | Fields and keys. |
| `syntax.attribute` | opaque | `terminal.ansi.yellow` | Attributes and decorators. |
| `syntax.tag` | opaque | `terminal.ansi.red` | HTML and JSX tag names, CSS selectors. |
| `syntax.punctuation` | opaque | `ui.text.muted` | Delimiters and separators. |
| `syntax.punctuation.bracket` | opaque | `syntax.punctuation` | Brackets. |
| `syntax.markup.heading` | opaque | `syntax.function` | Markdown headings. |
| `syntax.markup.bold` | opaque | `editor.foreground` | Bold text in markup. |
| `syntax.markup.italic` | opaque | `editor.foreground` | Italic text in markup. |
| `syntax.markup.link` | opaque | `terminal.ansi.cyan` | Links in markup. |

#### Density (`[density.compact]`, `[density.comfortable]`)

| Key | Range | Compact | Comfortable | Used for |
|---|---|---|---|---|
| `sidebar.row-height` | 1 to 1200 | 26 | 30 | Height of a sidebar row. |
| `tab.height` | 1 to 1200 | 30 | 34 | Height of a tab. |
| `pane.header-height` | 1 to 1200 | 30 | 36 | Height of a pane header. |
| `status-bar.height` | 1 to 1200 | 24 | 24 | Height of the status bar. |
| `spacing` | 0 to 1200 | 4 | 6 | Base spacing unit. |
| `radius.control` | 0 to 1000 | 6 | 6 | Corner radius of controls (buttons, rows, inputs). |
| `radius.card` | 0 to 1000 | 10 | 10 | Corner radius of cards, the composer, popovers and the palette. |
| `text.body.size` | 8 to 40 | 12 | 13 | UI body text size. |
| `text.secondary.size` | 8 to 40 | 11 | 12 | Secondary UI text size (subtitles, chips, hints). |
| `text.section.size` | 8 to 40 | 11 | 11 | Section label size. |
| `text.prose.size` | 8 to 40 | 15 | 15 | Prose size in the chat view and empty states. |
| `text.prose.line-height` | 0.8 to 3 | 1.6 | 1.6 | Prose line height as a multiple of its size. |
| `control.height` | 1 to 1200 | 28 | 28 | Height of buttons and single-line fields. |
| `terminal.padding` | 0 to 1200 | 8 | 12 | Space between a terminal view's edge and its text. |
| `terminal.block-gutter` | 0 to 16 | 3 | 4 | Width of the block gutter bar at a terminal's left edge. |

#### Metrics (`[metrics]`)

| Key | Range | Built-in | Used for |
|---|---|---|---|
| `titlebar.height` | 1 to 1200 | 36 | Height of the title bar and of the right sidebar's icon strip. |
| `radius.pill` | 0 to 1000 | 999 | Corner radius of pills, badges and round buttons. |
| `reading.width` | 240 to 1600 | 720 | Width of the chat's reading column. |
| `mascot.size` | 1 to 1200 | 96 | Size of the mascot in empty states. |
| `nav.text.size` | 8 to 40 | 13 | Text size of a nav row. |
| `nav.icon.size` | 8 to 40 | 16 | Icon size of a nav row. |
| `nav.line-height` | 8 to 1200 | 18 | Line height of a nav label. |
| `nav.padding-x` | 0 to 1200 | 8 | Horizontal padding of a nav row. |
| `nav.padding-y` | 0 to 1200 | 6 | Vertical padding of a nav row. |
| `nav.gap` | 0 to 1200 | 2 | Gap between nav rows. |
| `strip.button.size` | 1 to 1200 | 28 | Size of an icon button of the right strip. |
| `strip.gap` | 0 to 1200 | 4 | Gap between strip buttons. |
| `terminal.font-size` | 4 to 72 | 13 | Default terminal font size; `terminal.font_size` wins. |
| `terminal.line-height` | 0.8 to 3 | 1.2 | Default terminal line height; `terminal.line_height` wins. |
| `shadow.offset-x` | -128 to 128 | 0 | Horizontal offset of the shadow. |
| `shadow.offset-y` | -128 to 128 | 8 | Vertical offset of the shadow. |
| `shadow.blur` | 0 to 128 | 24 | Blur radius of the shadow. |
| `shadow.spread` | 0 to 128 | 0 | Spread of the shadow. |

#### Fonts (`[fonts]`)

| Key | Built-in | Used for |
|---|---|---|
| `ui` | `.SystemUIFont` | Chrome text. The first installed family wins. |
| `prose` | `.New York`, `New York`, `Georgia`, `.SystemUIFont` | Chat prose and empty-state copy. The first installed family wins. |
| `mono` | `Menlo` | Terminal default and monospaced text. The first installed family wins. |

<!-- tokens:end -->

## Icon packs

Install one directory under `<data dir>/icon-themes/<id>/` (never project auto-discovery):

```text
icon-themes/canopy-lines/
  icon-theme.toml
  icons/search.svg
  icons/close.svg
  LICENSE
```

```toml
schema_version = 1
id = "canopy-lines"
name = "Canopy Lines"
authors = ["Hutan Theme Authors"]
license = "MIT OR Apache-2.0"
extends = "builtin:phosphor"

[icons]
"nav.search" = "icons/search.svg"
"action.close" = "icons/close.svg"
```

Example `icons/search.svg`:

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
  <circle cx="10.5" cy="10.5" r="6.5"/>
  <path d="M15.5 15.5 21 21"/>
</svg>
```

Example `icons/close.svg`:

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round">
  <path d="M6 6 18 18M18 6 6 18"/>
</svg>
```

Select independently of colors, live:

```toml
[appearance]
icon_theme = "canopy-lines" # default: phosphor
```

Metadata uses the color-pack rules above; `appearance` is optional (normally omit it for currentColor). A declared light/dark polarity must match the window or selection warns and uses embedded Phosphor. Sparse packs require `extends = "builtin:phosphor"`; complete root packs use `root = true`, omit `extends` and cover every registered id. User packs can shadow `phosphor`, but `builtin:phosphor` always means immutable embedded content. Duplicate user ids disable all duplicates. Unknown ids warn and are ignored. Multiple ids may name the same SVG.

Omitted ids fall back individually: the two-icon example changes only the nav search and close controls, not `action.search`, other icons, colors, control sizes, focus, keyboard bindings or click handlers. Malformed SVG edits warn and preserve the active entry's last-good bytes; on first load they use the built-in for that id. Invalid metadata/TOML rejects the update (previous manifest retained when available); invalid initial packs are listed disabled. Missing selection retains the requested setting and recovers on installation. Errors use the existing Settings Problems list, with file and `icons.<id>` key.

The same debounced, bounded theme worker watches `icon-themes/`, pack folders and nested asset folders, including atomic saves. Validated normalized bytes are immutable; cache keys contain pack identity, SHA-256 content and semantic id. Only the selected pack retains user SVG bytes; inactive catalogs retain paths/hashes. Desktop keeps at most one retired raw generation (two 8 MiB snapshots plus built-in, below the 32 MiB raw limit). A separate 64 MiB LRU owns decoded/raster images and evicts their GPUI atlas entries; current-frame references live through the frame. No pack can force an intrinsic raster size.

### Artwork and security

- Use square positive `viewBox` (24×24 recommended; native Phosphor 256×256 works), padded for 14/16/20 pt. Only `currentColor` or `none` paint; the location supplies color. Expanded filled paths and strokes are both supported. Geometry never sets a hit target or changes a layout. Right-facing semantic assets such as `dock.right` are already oriented; Hutan never mirrors a user's asset.
- Accepted elements: svg, g, path, rect, circle, ellipse, line, polyline, polygon. Accepted attributes: numeric geometry/points/transforms, viewBox, fill/stroke, opacity, fill/clip rules and stroke width/cap/join/miterlimit. Intrinsic root width/height are ignored. Unsupported markup is rejected, not silently stripped. Only reconstructed safe XML is decoded.
- Refused: scripts, event attributes, DTD/entities/processing instructions, CSS/style, external/data/file URLs, images, text/fonts, foreign namespaces/objects, animation, filters, masks, patterns, use/href and `url(...)`. The loader does not fetch anything.
- Bounds: 256 pack candidates, 256 KiB manifests / 16 MiB per scan, 512 icon entries/files, asset subdirectory depth 4, 64 KiB per SVG / 8 MiB per pack. XML depth 16, 2,048 elements, 16,384 path segments, 32 KiB attributes, finite coordinates of absolute value ≤1e6, square viewBox side ≤4096. Paths and licenses use descriptor-relative confinement.
- Root built-in Phosphor Regular comes only from official `phosphor-icons/core`, pinned to `2b75f3ad12b420c9504ef05df8d2564a28f8500e`. Only catalog artwork is included; `assets/icon-themes/phosphor/LICENSE` retains its MIT notice, `PROVENANCE` records the revision and the baked right-sidebar mirror. Harness and Mawas identities use the catalog-approved complete Phosphor fallbacks; the Hutan silhouette is original MIT artwork, not an upstream trademark/logo.
- macOS traffic lights, native menu notation, app icon, menu-bar template PNGs and notification identity remain fixed system/product surfaces. `brand.menu-bar` is unsupported and warns. Colorful mascot illustrations and user image attachments are not monochrome product icons.
- Text exceptions: terminal/agent output, code, filenames/user text, prose punctuation, actual diff `+`/`−`/`±`, multiplication dimensions, and real keyboard keycaps (`⌘`, `⇧`, `⌥`, arrows, `↵`) remain text. Tooltip and status-strip sentences cannot embed artwork and keep their own prose marks: the `⚠` that opens the shared-checkout line of a placement tooltip, the `✓` of the History scan strip, and the `▾` that a sentence uses to point at a control. State words, counts, harness names and control labels remain beside icons; custom artwork never replaces their meaning.

### Semantic id reference

Generated from `IconId::ALL`; a drift test enforces this list. Offline schema: `docs/schemas/icon-theme-v1.json` (generated from the same registry; never downloaded).

<!-- icons:start -->
- `nav.search`
- `action.search`
- `nav.tasks`
- `panel.tasks`
- `nav.agents`
- `panel.agents-dashboard`
- `nav.mawas`
- `panel.assistant`
- `panel.history`
- `panel.changes`
- `action.show-changes`
- `panel.inspector`
- `panel.agents`
- `panel.resources`
- `action.group`
- `action.filter`
- `action.refresh`
- `action.settings`
- `nav.settings`
- `action.expand`
- `action.menu-down`
- `action.collapse`
- `action.sort-ascending`
- `action.back`
- `action.more`
- `action.add`
- `action.new-session`
- `action.sort`
- `panel.sessions`
- `nav.projects`
- `panel.mawas-memory`
- `nav.memory`
- `panel.notifications`
- `status.notifications`
- `nav.home`
- `nav.workspace`
- `status.workspace`
- `nav.project.repository`
- `nav.project.folder`
- `session.terminal`
- `harness.hutan`
- `session.harness`
- `session.direct`
- `harness.omp`
- `harness.claude`
- `harness.codex`
- `harness.unknown`
- `vcs.branch`
- `vcs.worktree`
- `vcs.main-checkout`
- `vcs.dirty`
- `vcs.clean`
- `vcs.shared-warning`
- `state.working`
- `state.blocked`
- `state.warning`
- `state.idle`
- `state.todo`
- `state.ready`
- `state.running`
- `state.completed`
- `state.failed`
- `state.interrupted`
- `state.cancelled`
- `state.disconnected`
- `state.stopped`
- `state.unknown`
- `state.unread`
- `state.approval`
- `status.approvals`
- `state.question`
- `state.review`
- `state.info`
- `status.attention`
- `status.connected`
- `status.connecting`
- `status.disconnected`
- `status.sessions`
- `status.density`
- `status.theme`
- `status.palette`
- `dock.left`
- `dock.right`
- `dock.bottom`
- `dock.compact`
- `dock.hide`
- `dock.expand`
- `action.disclose`
- `action.sort-descending`
- `action.start`
- `action.close`
- `action.interrupt`
- `action.stop`
- `action.approve`
- `action.deny`
- `action.open-session`
- `action.reveal-session`
- `action.open-worktree`
- `action.copy`
- `action.rename`
- `action.delete`
- `action.pin`
- `action.recover`
- `action.subagents`
- `action.forward`
- `action.add-context`
- `brand.hutan`
- `brand.mawas`
- `item.file`
<!-- icons:end -->
