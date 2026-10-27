//! The token registry: one exhaustive table of every color, density and shared metric a pack can
//! set, with its id, documentation, bounds and fallback. Nothing else in the workspace lists
//! tokens: the loader, the resolver, the docs reference and the tests all read these tables.
//!
//! Ids are quoted dotted keys of the manifest (`"ui.sidebar.background"`); the grammar is
//! [`valid_token_id`].

use super::color::Rgba;

/// How a color that a pack does not declare gets its value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fallback {
    /// No fallback: the token is required in a root pack (a sparse pack inherits it).
    Declared,
    /// The value of another token.
    Alias(Color),
    /// Another token at this fraction of its alpha.
    Alpha(Color, f32),
    /// Another token at one alpha in dark packs and another in light ones.
    AlphaByPolarity { of: Color, dark: f32, light: f32 },
    /// `from` moved `amount` of the way towards `toward`.
    Mix {
        from: Color,
        toward: Color,
        amount: f32,
    },
    /// Not drawn at all unless declared.
    Absent,
}

/// One row of the color table.
#[derive(Debug, Clone, Copy)]
pub struct ColorSpec {
    pub token: Color,
    pub id: &'static str,
    /// Text, glyph and terminal colors must be opaque; surfaces, borders, effects and tints may
    /// be translucent.
    pub opaque: bool,
    pub fallback: Fallback,
    pub doc: &'static str,
}

macro_rules! colors {
    ($( $variant:ident = $id:literal, $opaque:literal, $fallback:expr, $doc:literal; )*) => {
        /// Every color token. The order is the resolution order: a fallback only refers to
        /// tokens above it.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Color { $($variant),* }

        impl Color {
            /// All tokens in registry order.
            pub const ALL: &'static [Color] = &[$(Color::$variant),*];
            pub const COUNT: usize = Self::ALL.len();

            pub const fn index(self) -> usize {
                self as usize
            }

            pub const fn spec(self) -> &'static ColorSpec {
                &COLOR_SPECS[self as usize]
            }

            pub const fn id(self) -> &'static str {
                self.spec().id
            }
        }

        /// The color table, indexed by [`Color::index`].
        pub static COLOR_SPECS: [ColorSpec; Color::COUNT] = [
            $(ColorSpec {
                token: Color::$variant,
                id: $id,
                opaque: $opaque,
                fallback: $fallback,
                doc: $doc,
            }),*
        ];
    };
}

use Fallback::{Absent, Alias, Alpha, AlphaByPolarity, Declared, Mix};

colors! {
    // ---- chrome: the 16 fields of the old `Theme` plus the text on the accent ----
    Background = "ui.background", false, Declared, "Window, center panes and the active tab of a pane.";
    SidebarBackground = "ui.sidebar.background", false, Declared, "Left dock, the title bar, the tab strips and the status bar.";
    SidebarSelected = "ui.sidebar.selected", false, Declared, "Selected nav row and active project card of the left sidebar.";
    SidebarRightBackground = "ui.sidebar-right.background", false, Declared, "The right sidebar surface.";
    SidebarRightSelected = "ui.sidebar-right.selected", false, Declared, "Hover and selected rows of the right sidebar.";
    Surface = "ui.surface", false, Declared, "Cards, composer, popovers, palette and modals.";
    SurfaceHover = "ui.surface.hover", false, Declared, "Row and button hover fill; inline code background in prose.";
    Border = "ui.border", false, Declared, "Hairlines (1 px). Usually translucent in dark packs.";
    Foreground = "ui.foreground", true, Declared, "Primary text.";
    TextMuted = "ui.text.muted", true, Declared, "Secondary text and icons.";
    TextFaint = "ui.text.faint", true, Declared, "Disabled text.";
    Accent = "ui.accent", true, Declared, "Primary buttons, selection, focus, links: the brand color.";
    AccentSoft = "ui.accent.soft", false, Declared, "Selected row and chips.";
    OnAccent = "ui.on-accent", true, Declared, "Text on an accent fill (primary buttons, the send button).";
    StateSuccess = "state.success", true, Declared, "Success: completed work, added lines, a connected daemon.";
    StateWarning = "state.warning", true, Declared, "Warning: blocked agents, pending approvals, renamed files.";
    StateError = "state.error", true, Declared, "Error: failed sessions, removed lines, a stopped daemon.";
    Shadow = "ui.shadow", false, Declared, "Color of the one soft shadow of popovers, the palette and modals.";
    Scrim = "ui.scrim", false, Declared, "Dimming layer behind modals and sheets.";

    // ---- terminal: the 4 core colors and the 16 ANSI colors ----
    TerminalForeground = "terminal.foreground", true, Declared, "Default text color of terminals.";
    TerminalBackground = "terminal.background", true, Declared, "Default background of terminals.";
    TerminalCursor = "terminal.cursor", true, Declared, "Cursor color.";
    TerminalSelectionBackground = "terminal.selection.background", true, Declared, "Background of selected cells.";
    AnsiBlack = "terminal.ansi.black", true, Declared, "ANSI color 0.";
    AnsiRed = "terminal.ansi.red", true, Declared, "ANSI color 1.";
    AnsiGreen = "terminal.ansi.green", true, Declared, "ANSI color 2.";
    AnsiYellow = "terminal.ansi.yellow", true, Declared, "ANSI color 3.";
    AnsiBlue = "terminal.ansi.blue", true, Declared, "ANSI color 4.";
    AnsiMagenta = "terminal.ansi.magenta", true, Declared, "ANSI color 5.";
    AnsiCyan = "terminal.ansi.cyan", true, Declared, "ANSI color 6.";
    AnsiWhite = "terminal.ansi.white", true, Declared, "ANSI color 7.";
    AnsiBrightBlack = "terminal.ansi.bright-black", true, Declared, "ANSI color 8.";
    AnsiBrightRed = "terminal.ansi.bright-red", true, Declared, "ANSI color 9.";
    AnsiBrightGreen = "terminal.ansi.bright-green", true, Declared, "ANSI color 10.";
    AnsiBrightYellow = "terminal.ansi.bright-yellow", true, Declared, "ANSI color 11.";
    AnsiBrightBlue = "terminal.ansi.bright-blue", true, Declared, "ANSI color 12.";
    AnsiBrightMagenta = "terminal.ansi.bright-magenta", true, Declared, "ANSI color 13.";
    AnsiBrightCyan = "terminal.ansi.bright-cyan", true, Declared, "ANSI color 14.";
    AnsiBrightWhite = "terminal.ansi.bright-white", true, Declared, "ANSI color 15.";

    // ---- optional roles; each falls back to something above ----
    SidebarHover = "ui.sidebar.hover", false, Alpha(Color::Foreground, 0.08), "Hover fill of the left nav rows.";
    DropTarget = "ui.drop-target", false, AlphaByPolarity { of: Color::Accent, dark: 0.28, light: 0.22 }, "Drag-and-drop target highlight.";
    Focus = "ui.focus", true, Absent, "The 2 px keyboard focus ring, drawn only while focus came from the keyboard; absent: the macOS keyboard focus indicator color, read by the desktop at runtime.";
    StateWorking = "state.working", true, Alias(Color::Accent), "A working or starting session.";
    StateStarting = "state.starting", true, Alias(Color::Accent), "A session that is starting.";
    StateIdle = "state.idle", true, Alias(Color::TextMuted), "An idle session.";
    StateUnknown = "state.unknown", true, Alias(Color::TextMuted), "A session or agent in an unknown state.";
    StateInterrupted = "state.interrupted", true, Alias(Color::TextMuted), "An interrupted agent.";
    StateDisconnected = "state.disconnected", true, Alias(Color::TextMuted), "A disconnected agent.";
    StateBlocked = "state.blocked", true, Alias(Color::StateWarning), "A blocked agent.";
    StateApproval = "state.approval", true, Alias(Color::StateWarning), "A session waiting for an approval.";
    StateQuestion = "state.question", true, Alias(Color::StateWarning), "A session waiting for an answer.";
    StateNeedsYou = "state.needs-you", true, Alias(Color::StateWarning), "The aggregate needs-you indicator.";
    StateCompleted = "state.completed", true, Alias(Color::StateSuccess), "A completed session or agent.";
    StateFailed = "state.failed", true, Alias(Color::StateError), "A failed session or agent.";
    DiffAdded = "diff.added", true, Alias(Color::StateSuccess), "Added files and lines: text.";
    DiffRemoved = "diff.removed", true, Alias(Color::StateError), "Removed files and lines: text.";
    DiffModified = "diff.modified", true, Alias(Color::Accent), "Modified files: text.";
    DiffRenamed = "diff.renamed", true, Alias(Color::StateWarning), "Renamed files: text.";
    DiffAddedBackground = "diff.added.background", false, Alpha(Color::DiffAdded, 0.16), "Row tint of added lines.";
    DiffRemovedBackground = "diff.removed.background", false, Alpha(Color::DiffRemoved, 0.16), "Row tint of removed lines.";
    DiffHunkBackground = "diff.hunk.background", false, Alpha(Color::Accent, 0.10), "Row tint of hunk headers.";
    DiffHunkForeground = "diff.hunk.foreground", true, Alias(Color::TextMuted), "Text of hunk headers.";
    TerminalCursorForeground = "terminal.cursor.foreground", true, Alias(Color::TerminalBackground), "The glyph under a block cursor.";
    TerminalSelectionForeground = "terminal.selection.foreground", true, Absent, "Text color of selected cells; absent keeps each cell's own foreground.";
    BlockSuccess = "terminal.block.success", true, Alias(Color::StateSuccess), "Gutter bar and chip of a command that exited 0.";
    BlockError = "terminal.block.error", true, Alias(Color::StateError), "Gutter bar and chip of a command that failed.";
    BlockNeutral = "terminal.block.neutral", true, Alias(Color::TextMuted), "Running commands and commands without an exit code.";
    BlockAccent = "terminal.block.accent", true, Alias(Color::Accent), "Accent of the highlighted block.";
    BlockSeparator = "terminal.block.separator", false, Mix { from: Color::TerminalBackground, toward: Color::TerminalForeground, amount: 0.16 }, "Line between two blocks.";
    BlockHighlight = "terminal.block.highlight", false, Alpha(Color::BlockAccent, 0.13), "Tint of the highlighted block.";
    BlockHover = "terminal.block.hover", false, Alpha(Color::BlockNeutral, 0.07), "Tint of the hovered block.";
    BlockFolded = "terminal.block.folded", false, Mix { from: Color::TerminalBackground, toward: Color::TerminalForeground, amount: 0.07 }, "Fill of a folded block's summary row.";
    MarkdownLink = "markdown.link", true, Alias(Color::Accent), "Links in chat messages.";
    MarkdownInlineCodeBackground = "markdown.inline-code.background", false, Alias(Color::SurfaceHover), "Background of inline code in chat messages.";

    // ---- editor surface (E-4): every one derived, so a sparse pack gets a complete editor ----
    EditorBackground = "editor.background", false, Alias(Color::Background), "Background of the code editor.";
    EditorForeground = "editor.foreground", true, Alias(Color::Foreground), "Default text of the code editor: code that no syntax token colors.";
    EditorGutterBackground = "editor.gutter.background", false, Alias(Color::EditorBackground), "Background of the line-number gutter.";
    EditorGutterForeground = "editor.gutter.foreground", true, Mix { from: Color::EditorForeground, toward: Color::EditorBackground, amount: 0.4 }, "Line numbers.";
    EditorLineNumberActive = "editor.line-number.active", true, Alias(Color::EditorForeground), "The line number of the cursor's line.";
    EditorCurrentLine = "editor.current-line", false, AlphaByPolarity { of: Color::EditorForeground, dark: 0.05, light: 0.04 }, "Tint of the cursor's line.";
    EditorSelection = "editor.selection", false, AlphaByPolarity { of: Color::Accent, dark: 0.32, light: 0.26 }, "Background of selected text in the focused editor.";
    EditorSelectionInactive = "editor.selection.inactive", false, Alpha(Color::EditorSelection, 0.5), "Background of selected text in an editor that does not have focus.";
    EditorCursor = "editor.cursor", true, Alias(Color::Accent), "The text cursor.";
    EditorWhitespace = "editor.whitespace", false, Alpha(Color::EditorForeground, 0.25), "Visible whitespace marks (spaces, tabs).";
    EditorIndentGuide = "editor.indent-guide", false, Alpha(Color::EditorForeground, 0.10), "Vertical indentation guides.";
    EditorBracketMatch = "editor.bracket-match", false, Alpha(Color::Accent, 0.28), "Background of a bracket and its partner.";
    EditorSearchMatch = "editor.search-match", false, Alpha(Color::StateWarning, 0.28), "Background of every find match.";
    EditorSearchMatchActive = "editor.search-match.active", false, Alpha(Color::StateWarning, 0.60), "Background of the current find match.";
    EditorDiagnosticError = "editor.diagnostic.error", true, Alias(Color::StateError), "Underline of an error diagnostic.";
    EditorDiagnosticWarning = "editor.diagnostic.warning", true, Alias(Color::StateWarning), "Underline of a warning diagnostic.";
    EditorDiagnosticInfo = "editor.diagnostic.info", true, Alias(Color::Accent), "Underline of an information diagnostic.";
    EditorDiagnosticHint = "editor.diagnostic.hint", true, Alias(Color::TextMuted), "Underline of a hint diagnostic.";
    EditorDiffGutterAdded = "editor.diff-gutter.added", true, Alias(Color::DiffAdded), "Gutter bar of lines added since the last commit.";
    EditorDiffGutterModified = "editor.diff-gutter.modified", true, Alias(Color::DiffModified), "Gutter bar of lines modified since the last commit.";
    EditorDiffGutterRemoved = "editor.diff-gutter.removed", true, Alias(Color::DiffRemoved), "Gutter mark where lines were removed since the last commit.";
    EditorAgentChanged = "editor.agent-changed", false, AlphaByPolarity { of: Color::Accent, dark: 0.20, light: 0.16 }, "Background of lines an agent changed.";

    // ---- buttons (UX-BUTTONS): the states of `ui::Button`; every one derived ----
    ButtonPrimaryHover = "button.primary.hover", false, Alpha(Color::Accent, 0.88), "Fill of a primary button under the pointer.";
    ButtonPrimaryPressed = "button.primary.pressed", false, Alpha(Color::Accent, 0.76), "Fill of a primary button while it is pressed.";
    ButtonHover = "button.hover", false, Alias(Color::SurfaceHover), "Fill of a secondary, ghost or icon button under the pointer.";
    ButtonPressed = "button.pressed", false, Mix { from: Color::SurfaceHover, toward: Color::Foreground, amount: 0.12 }, "Fill of a secondary, ghost or icon button while it is pressed: one step past the hover.";
    ButtonDestructiveHover = "button.destructive.hover", false, Alpha(Color::StateError, 0.12), "Fill of a destructive button under the pointer (its text and border are `state.error`).";
    ButtonDestructivePressed = "button.destructive.pressed", false, Alpha(Color::StateError, 0.22), "Fill of a destructive button while it is pressed.";

    // ---- scrollbars (SCROLLBARS): the thumb and track every scroll surface draws; every one derived ----
    ScrollbarThumb = "ui.scrollbar.thumb", false, Alpha(Color::Foreground, 0.28), "Thumb of a scrollbar at rest.";
    ScrollbarThumbActive = "ui.scrollbar.thumb.active", false, Alpha(Color::Foreground, 0.5), "Thumb of a scrollbar under the pointer or while it is dragged.";
    ScrollbarTrack = "ui.scrollbar.track", false, Alpha(Color::Foreground, 0.07), "Track behind the thumb: shown while the pointer is near the edge, and always with `appearance.scrollbars = always`; the markers of the editor's track lie on it.";

    // ---- syntax (E-4): the 22 names of `hutan_syntax::Token`, in its order. Text colors, so
    // opaque; the defaults are the normal ANSI colors, which the terminal contrast check holds
    // to 4.5:1, and the muted and editor foregrounds ----
    SyntaxComment = "syntax.comment", true, Alias(Color::TextMuted), "Comments.";
    SyntaxString = "syntax.string", true, Alias(Color::AnsiGreen), "Strings, character literals, raw text and dates.";
    SyntaxStringEscape = "syntax.string.escape", true, Alias(Color::AnsiCyan), "Escape sequences inside strings.";
    SyntaxNumber = "syntax.number", true, Alias(Color::AnsiYellow), "Numbers.";
    SyntaxConstant = "syntax.constant", true, Alias(Color::AnsiYellow), "Constants: `true`, `null`, enum-like names.";
    SyntaxKeyword = "syntax.keyword", true, Alias(Color::AnsiMagenta), "Keywords, `self` and `this`.";
    SyntaxOperator = "syntax.operator", true, Alias(Color::EditorForeground), "Operators.";
    SyntaxFunction = "syntax.function", true, Alias(Color::AnsiBlue), "Functions and macros: definitions and calls.";
    SyntaxFunctionMethod = "syntax.function.method", true, Alias(Color::SyntaxFunction), "Methods.";
    SyntaxType = "syntax.type", true, Alias(Color::AnsiCyan), "Types, constructors, namespaces and lifetimes.";
    SyntaxTypeBuiltin = "syntax.type.builtin", true, Alias(Color::SyntaxType), "Built-in types.";
    SyntaxVariable = "syntax.variable", true, Alias(Color::EditorForeground), "Variables.";
    SyntaxVariableParameter = "syntax.variable.parameter", true, Alias(Color::SyntaxVariable), "Function parameters.";
    SyntaxProperty = "syntax.property", true, Alias(Color::SyntaxVariable), "Fields and keys.";
    SyntaxAttribute = "syntax.attribute", true, Alias(Color::AnsiYellow), "Attributes and decorators.";
    SyntaxTag = "syntax.tag", true, Alias(Color::AnsiRed), "HTML and JSX tag names, CSS selectors.";
    SyntaxPunctuation = "syntax.punctuation", true, Alias(Color::TextMuted), "Delimiters and separators.";
    SyntaxPunctuationBracket = "syntax.punctuation.bracket", true, Alias(Color::SyntaxPunctuation), "Brackets.";
    SyntaxMarkupHeading = "syntax.markup.heading", true, Alias(Color::SyntaxFunction), "Markdown headings.";
    SyntaxMarkupBold = "syntax.markup.bold", true, Alias(Color::EditorForeground), "Bold text in markup.";
    SyntaxMarkupItalic = "syntax.markup.italic", true, Alias(Color::EditorForeground), "Italic text in markup.";
    SyntaxMarkupLink = "syntax.markup.link", true, Alias(Color::AnsiCyan), "Links in markup.";
}

impl Color {
    /// The 16 ANSI colors in index order.
    pub const ANSI: [Color; 16] = [
        Color::AnsiBlack,
        Color::AnsiRed,
        Color::AnsiGreen,
        Color::AnsiYellow,
        Color::AnsiBlue,
        Color::AnsiMagenta,
        Color::AnsiCyan,
        Color::AnsiWhite,
        Color::AnsiBrightBlack,
        Color::AnsiBrightRed,
        Color::AnsiBrightGreen,
        Color::AnsiBrightYellow,
        Color::AnsiBrightBlue,
        Color::AnsiBrightMagenta,
        Color::AnsiBrightCyan,
        Color::AnsiBrightWhite,
    ];

    /// The `syntax.*` tokens in the order of `hutan_syntax::Token::ALL` (a test holds the two
    /// lists equal), so a renderer indexes a table of them by the token's discriminant.
    pub const SYNTAX: [Color; 22] = [
        Color::SyntaxComment,
        Color::SyntaxString,
        Color::SyntaxStringEscape,
        Color::SyntaxNumber,
        Color::SyntaxConstant,
        Color::SyntaxKeyword,
        Color::SyntaxOperator,
        Color::SyntaxFunction,
        Color::SyntaxFunctionMethod,
        Color::SyntaxType,
        Color::SyntaxTypeBuiltin,
        Color::SyntaxVariable,
        Color::SyntaxVariableParameter,
        Color::SyntaxProperty,
        Color::SyntaxAttribute,
        Color::SyntaxTag,
        Color::SyntaxPunctuation,
        Color::SyntaxPunctuationBracket,
        Color::SyntaxMarkupHeading,
        Color::SyntaxMarkupBold,
        Color::SyntaxMarkupItalic,
        Color::SyntaxMarkupLink,
    ];

    /// The token with this manifest id.
    pub fn from_id(id: &str) -> Option<Color> {
        Self::ALL.iter().copied().find(|c| c.id() == id)
    }

    /// Whether a root pack must declare it.
    pub fn is_required(self) -> bool {
        matches!(self.spec().fallback, Fallback::Declared)
    }

    /// The color a fallback produces, given the values of the tokens above `self`.
    pub(super) fn derive(self, values: &[Rgba], dark: bool) -> Option<Rgba> {
        let at = |c: Color| values[c.index()];
        match self.spec().fallback {
            Fallback::Declared | Fallback::Absent => None,
            Fallback::Alias(of) => Some(at(of)),
            Fallback::Alpha(of, alpha) => Some(at(of).with_alpha(alpha)),
            Fallback::AlphaByPolarity {
                of,
                dark: d,
                light: l,
            } => Some(at(of).with_alpha(if dark { d } else { l })),
            Fallback::Mix {
                from,
                toward,
                amount,
            } => Some(at(from).mix(at(toward), amount)),
        }
    }
}

/// What a number token measures, for its bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberScope {
    /// One value per density (`[density.compact]`, `[density.comfortable]`).
    Density,
    /// One value for both densities (`[metrics]`).
    Metric,
}

/// One row of the density or metric tables.
#[derive(Debug, Clone, Copy)]
pub struct NumberSpec {
    pub id: &'static str,
    pub scope: NumberScope,
    pub min: f32,
    pub max: f32,
    pub doc: &'static str,
}

macro_rules! numbers {
    ($name:ident, $specs:ident, $scope:expr; $( $variant:ident = $id:literal, $min:expr, $max:expr, $doc:literal; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name { $($variant),* }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),*];
            pub const COUNT: usize = Self::ALL.len();

            pub const fn index(self) -> usize {
                self as usize
            }

            pub const fn spec(self) -> &'static NumberSpec {
                &$specs[self as usize]
            }

            pub const fn id(self) -> &'static str {
                self.spec().id
            }

            pub fn from_id(id: &str) -> Option<$name> {
                Self::ALL.iter().copied().find(|t| t.id() == id)
            }
        }

        pub static $specs: [NumberSpec; $name::COUNT] = [
            $(NumberSpec { id: $id, scope: $scope, min: $min, max: $max, doc: $doc }),*
        ];
    };
}

numbers! {
    DensityKey, DENSITY_SPECS, NumberScope::Density;
    SidebarRowHeight = "sidebar.row-height", 1.0, 1200.0, "Height of a sidebar row.";
    TabHeight = "tab.height", 1.0, 1200.0, "Height of a tab.";
    PaneHeaderHeight = "pane.header-height", 1.0, 1200.0, "Height of a pane header.";
    StatusBarHeight = "status-bar.height", 1.0, 1200.0, "Height of the status bar.";
    Spacing = "spacing", 0.0, 1200.0, "Base spacing unit.";
    RadiusControl = "radius.control", 0.0, 1000.0, "Corner radius of controls (buttons, rows, inputs).";
    RadiusCard = "radius.card", 0.0, 1000.0, "Corner radius of cards, the composer, popovers and the palette.";
    TextBodySize = "text.body.size", 8.0, 40.0, "UI body text size.";
    TextSecondarySize = "text.secondary.size", 8.0, 40.0, "Secondary UI text size (subtitles, chips, hints).";
    TextSectionSize = "text.section.size", 8.0, 40.0, "Section label size.";
    TextProseSize = "text.prose.size", 8.0, 40.0, "Prose size in the chat view and empty states.";
    TextProseLineHeight = "text.prose.line-height", 0.8, 3.0, "Prose line height as a multiple of its size.";
    ControlHeight = "control.height", 1.0, 1200.0, "Height of buttons and single-line fields.";
    ButtonHeightCompact = "button.height-compact", 1.0, 1200.0, "Height of a compact button (inline, card footers).";
    TerminalPadding = "terminal.padding", 0.0, 1200.0, "Space between a terminal view's edge and its text.";
    TerminalBlockGutter = "terminal.block-gutter", 0.0, 16.0, "Width of the block gutter bar at a terminal's left edge.";
}

numbers! {
    Metric, METRIC_SPECS, NumberScope::Metric;
    TitlebarHeight = "titlebar.height", 1.0, 1200.0, "Height of the title bar and of the right sidebar's icon strip.";
    RadiusPill = "radius.pill", 0.0, 1000.0, "Corner radius of pills, badges and round buttons.";
    ReadingWidth = "reading.width", 240.0, 1600.0, "Width of the chat's reading column.";
    MascotSize = "mascot.size", 1.0, 1200.0, "Size of the mascot in empty states.";
    NavTextSize = "nav.text.size", 8.0, 40.0, "Text size of a nav row.";
    NavIconSize = "nav.icon.size", 8.0, 40.0, "Icon size of a nav row.";
    NavLineHeight = "nav.line-height", 8.0, 1200.0, "Line height of a nav label.";
    NavPaddingX = "nav.padding-x", 0.0, 1200.0, "Horizontal padding of a nav row.";
    NavPaddingY = "nav.padding-y", 0.0, 1200.0, "Vertical padding of a nav row.";
    NavGap = "nav.gap", 0.0, 1200.0, "Gap between nav rows.";
    StripButtonSize = "strip.button.size", 1.0, 1200.0, "Size of an icon button of the right strip.";
    StripGap = "strip.gap", 0.0, 1200.0, "Gap between strip buttons.";
    ButtonPaddingX = "button.padding-x", 0.0, 1200.0, "Horizontal padding of a default button.";
    ButtonPaddingXCompact = "button.padding-x-compact", 0.0, 1200.0, "Horizontal padding of a compact button.";
    ButtonIconHit = "button.icon.hit", 1.0, 1200.0, "Side of an icon button's hit region; the hover fill is 4 px smaller.";
    ButtonIconSize = "button.icon.size", 8.0, 64.0, "Glyph size inside an icon button.";
    ButtonDisabledOpacity = "button.disabled-opacity", 0.1, 1.0, "Opacity of a disabled button.";
    TerminalFontSize = "terminal.font-size", 4.0, 72.0, "Default terminal font size; `terminal.font_size` wins.";
    TerminalLineHeight = "terminal.line-height", 0.8, 3.0, "Default terminal line height; `terminal.line_height` wins.";
    ShadowOffsetX = "shadow.offset-x", -128.0, 128.0, "Horizontal offset of the shadow.";
    ShadowOffsetY = "shadow.offset-y", -128.0, 128.0, "Vertical offset of the shadow.";
    ShadowBlur = "shadow.blur", 0.0, 128.0, "Blur radius of the shadow.";
    ShadowSpread = "shadow.spread", 0.0, 128.0, "Spread of the shadow.";
}

/// A font role of `[fonts]`: an ordered list of installed family names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontRole {
    Ui,
    Prose,
    Mono,
}

impl FontRole {
    pub const ALL: [FontRole; 3] = [FontRole::Ui, FontRole::Prose, FontRole::Mono];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Prose => "prose",
            Self::Mono => "mono",
        }
    }

    pub const fn doc(self) -> &'static str {
        match self {
            Self::Ui => "Chrome text. The first installed family wins.",
            Self::Prose => {
                "Chat prose and empty-state copy: the UI face unless a pack names another. The first installed family wins."
            }
            Self::Mono => "Terminal default and monospaced text. The first installed family wins.",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.id() == id)
    }
}

/// The grammar of token ids: `^[a-z][a-z0-9]*(?:[.-][a-z0-9]+)*$`, at most 96 bytes.
pub fn valid_token_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 96 || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    let mut previous_separator = false;
    for &b in &bytes[1..] {
        match b {
            b'a'..=b'z' | b'0'..=b'9' => previous_separator = false,
            b'.' | b'-' if !previous_separator => previous_separator = true,
            _ => return false,
        }
    }
    !previous_separator
}

/// The grammar of pack ids: `^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$`, 1-64 bytes.
pub fn valid_pack_id(id: &str) -> bool {
    valid_token_id(id) && id.len() <= 64 && !id.contains('.')
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_token_is_listed_once_with_a_valid_unique_id() {
        let mut seen = HashSet::new();
        for (i, spec) in COLOR_SPECS.iter().enumerate() {
            assert_eq!(spec.token.index(), i, "{}", spec.id);
            assert!(valid_token_id(spec.id), "{}", spec.id);
            assert!(seen.insert(spec.id), "duplicate id {}", spec.id);
            assert!(!spec.doc.is_empty(), "{} has no docs", spec.id);
            assert_eq!(Color::from_id(spec.id), Some(spec.token));
        }
        for spec in DENSITY_SPECS.iter().chain(METRIC_SPECS.iter()) {
            assert!(valid_token_id(spec.id), "{}", spec.id);
            assert!(!spec.doc.is_empty(), "{} has no docs", spec.id);
            assert!(spec.min <= spec.max, "{}", spec.id);
        }
        let density: HashSet<_> = DENSITY_SPECS.iter().map(|s| s.id).collect();
        assert_eq!(density.len(), DensityKey::COUNT);
        let metrics: HashSet<_> = METRIC_SPECS.iter().map(|s| s.id).collect();
        assert_eq!(metrics.len(), Metric::COUNT);
        assert_eq!(Color::from_id("ui.nope"), None);
    }

    #[test]
    fn the_inventory_has_the_documented_size() {
        let required = Color::ALL.iter().filter(|c| c.is_required()).count();
        // 17 chrome colors + shadow + scrim, 4 terminal core colors + 16 ANSI colors.
        assert_eq!(required, 17 + 2 + 4 + 16);
        assert_eq!(DensityKey::COUNT, 16);
        assert_eq!(Metric::COUNT, 23);
    }

    #[test]
    fn a_fallback_only_refers_to_tokens_above_it() {
        for spec in &COLOR_SPECS {
            let refs: Vec<Color> = match spec.fallback {
                Fallback::Declared | Fallback::Absent => vec![],
                Fallback::Alias(of)
                | Fallback::Alpha(of, _)
                | Fallback::AlphaByPolarity { of, .. } => vec![of],
                Fallback::Mix { from, toward, .. } => vec![from, toward],
            };
            for of in refs {
                assert!(of < spec.token, "{} refers to {}", spec.id, of.id());
            }
        }
    }

    #[test]
    fn opaque_roles_are_the_text_and_terminal_ones() {
        for id in [
            "ui.foreground",
            "ui.text.muted",
            "ui.on-accent",
            "state.error",
            "terminal.foreground",
            "terminal.selection.background",
            "terminal.ansi.red",
            "diff.hunk.foreground",
        ] {
            assert!(Color::from_id(id).unwrap().spec().opaque, "{id}");
        }
        for id in [
            "ui.border",
            "ui.shadow",
            "ui.scrim",
            "diff.added.background",
            "terminal.block.highlight",
        ] {
            assert!(!Color::from_id(id).unwrap().spec().opaque, "{id}");
        }
    }

    #[test]
    fn the_grammar_matches_the_contract() {
        for ok in [
            "a",
            "ui.background",
            "ui.sidebar-right.selected",
            "a1.b-2.c3",
        ] {
            assert!(valid_token_id(ok), "{ok}");
        }
        for bad in [
            "",
            "1a",
            "-a",
            "a.",
            "a..b",
            "a-.b",
            "A",
            "a_b",
            "a b",
            "ui.Background",
            ".a",
            "a.-b",
        ] {
            assert!(!valid_token_id(bad), "{bad:?}");
        }
        assert!(!valid_token_id(&"a".repeat(97)));
        assert!(valid_token_id(&"a".repeat(96)));
        assert!(valid_pack_id("canopy-night"));
        assert!(!valid_pack_id("canopy.night"));
        assert!(!valid_pack_id(&"a".repeat(65)));
    }
}
