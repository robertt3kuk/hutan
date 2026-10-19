# Themes

Hutan's look is data. A **color pack** is one TOML file that sets the chrome, the terminal palette, state and diff colors, fonts, density and effects. `hutan-dark` and `hutan-light` are packs too: they are embedded in the binary (`assets/themes/<id>/theme.toml`) and parsed by the same loader as yours. Decision and format contract: ADR 0018 and `docs/spikes/2026-10-20-theme-packs.md`. Icon packs are a separate, later slice (TH-2); this page is about colors, fonts and density.

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
- Unknown keys and tokens warn and are ignored. `syntax.*` is reserved for the editor and reported as unsupported. A wrong type, a malformed color, a number out of range or an invalid `extends` is an error and the pack is not loaded.
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

## Hot reload

One watcher per application watches `themes/` (creation, removal, single-file packs) and each pack folder (edits in place; atomic-save rename works). Events go through a bounded queue and a debounce (100 ms quiet, 500 ms at most) into one worker that rescans; a flood coalesces into one rescan. Editing the active pack restyles chrome, chat, diffs, terminal rows, fonts and density without a restart, and the daemon learns the new terminal colors once. An edit that changes nothing publishes nothing, and editing a pack that is not in use changes only the catalog (pickers, Problems): no view is invalidated.

- An invalid edit of a pack that was valid keeps the **last good** version and adds Problems (`kept the previous version of theme ...`). A pack that was never valid is listed disabled and cannot be selected. A removed pack disappears; one that reappears loads again.
- If the watcher cannot start, hot reload is off (a warning in the log); everything else works.

## Limits and safety

| Limit | Value |
|---|---|
| Manifest size | 256 KiB (checked on the real read length too); 16 MiB of manifests per scan |
| Candidate entries per scan | 256 (the rest are reported, not read) |
| Color, density and metric entries | at most 512 per manifest |
| Table nesting | 5 levels |
| Diagnostics kept | 512 plus one summary line |
| Font families per role | 8, each at most 120 characters |

Paths (`license_file`) are normalized relative UTF-8 paths with no `..`, absolute, `~`, `:`, backslash or empty component. Every component is checked with `lstat` and refused if it is a symlink, the file is opened with `O_NOFOLLOW`, must be a regular file, and the identity of every component is compared again after the read, so a path swapped for a symlink mid-read fails instead of leaking another file.

## Problems and contrast

Pack and selection problems appear in the Settings Problems list (the same list as settings errors) with the file, the key (`colors.ui.border`, `appearance.dark_theme`) and the reason. Contrast is advice only (key `contrast`, WCAG 2.2 ratios, translucent colors composed over what they are drawn on): a user pack is reported for the pairs it makes worse than its base. The built-ins' own known shortfalls (for example white on the orange accent) are a reviewed baseline in `crates/settings/tests/themes.rs`; they are reported nowhere else and nothing is repaired.

## Differences from the hard-coded look

The built-in packs reproduce the previous colors, sizes and fonts. The only deliberate differences are 8-bit alpha quantization of values that were fractions: the dark hairline `#ffffff12` is alpha 0.0706 (was 0.07), the shadow alphas are 0.1216 (light) and 0.4 (dark) (were 0.12, 0.40), and the scrim 0.2784 and 0.5020 (were 0.28, 0.50). Derived tints keep the exact old fractions (0.08, 0.10, 0.16, 0.22, 0.28, 0.13, 0.07). The status bar is 24 px in both densities, as drawn; the unused 22 / 26 table is gone.

Titlebar height, nav row and strip sizes (`titlebar.height`, `nav.*`, `strip.*`) are loaded, validated and resolved, but the layout code that draws them (`render.rs`, `sidebars.rs`) still reads constants equal to the built-in values (a test keeps `titlebar.height` and the constant equal); they take effect once the dock work that owns those files lands.

## Token reference

Generated from the token registry (`crates/settings/src/themes/tokens.rs`) by `UPDATE_GOLDEN=1 cargo nextest run -p hutan-settings --test themes`; a test fails if this section drifts.

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
| `ui.text.faint` | opaque | required in a root pack | Placeholders and disabled text. |
| `ui.accent` | opaque | required in a root pack | Primary buttons, selection, focus, links: the brand color. |
| `ui.accent.soft` | may be translucent | required in a root pack | Selected row, chips, the user message bubble. |
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

#### Density (`[density.compact]`, `[density.comfortable]`)

| Key | Range | Compact | Comfortable | Used for |
|---|---|---|---|---|
| `sidebar.row-height` | 1 to 1200 | 26 | 30 | Height of a sidebar row. |
| `tab.height` | 1 to 1200 | 30 | 34 | Height of a tab. |
| `pane.header-height` | 1 to 1200 | 30 | 36 | Height of a pane header. |
| `status-bar.height` | 1 to 1200 | 24 | 24 | Height of the status bar. |
| `spacing` | 0 to 1200 | 4 | 6 | Base spacing unit. |
| `radius.control` | 0 to 1000 | 6 | 8 | Corner radius of controls (buttons, rows, inputs). |
| `radius.card` | 0 to 1000 | 10 | 12 | Corner radius of cards, the composer, popovers and the palette. |
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
