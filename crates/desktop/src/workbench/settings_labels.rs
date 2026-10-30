//! What the settings sheet says: the user-facing title and help of every namespace and field,
//! and a label for every value of a choice. The schemas' doc comments are written for people who
//! edit `settings.toml` (they name keys, types and files); the sheet shows these instead, and
//! `docs/design.md` § Settings keeps the same wording. Pure data: no GPUI.
//!
//! A field without an entry here falls back to its key in Title Case and the schema's doc; the
//! tests fail for every field and choice of the built-in namespaces that has no entry, so the
//! fallback only ever shows for a namespace somebody added without wording.

/// The text of one field.
struct FieldText {
    namespace: &'static str,
    key: &'static str,
    title: &'static str,
    help: &'static str,
    /// `(value, label)` of every choice of a dropdown.
    values: &'static [(&'static str, &'static str)],
}

const fn field(
    namespace: &'static str,
    key: &'static str,
    title: &'static str,
    help: &'static str,
) -> FieldText {
    FieldText {
        namespace,
        key,
        title,
        help,
        values: &[],
    }
}

const fn choice(
    namespace: &'static str,
    key: &'static str,
    title: &'static str,
    help: &'static str,
    values: &'static [(&'static str, &'static str)],
) -> FieldText {
    FieldText {
        namespace,
        key,
        title,
        help,
        values,
    }
}

/// The title of a namespace in the sheet's list and header.
const NAMESPACES: &[(&str, &str)] = &[
    ("appearance", "Appearance"),
    ("app", "General"),
    ("data", "Data & logs"),
    ("keymap", "Keyboard"),
    ("terminal", "Terminal"),
    ("layout", "Layout"),
    ("tabs", "Tabs"),
    ("panels.sessions", "Projects Sidebar"),
    ("sidebar", "Sidebar organization"),
    ("panels.inspector", "Requests"),
    ("panels.files", "Files"),
    ("panels.problems", "Problems"),
    ("panels.instructions", "Instructions"),
    ("panels.context", "Context"),
    ("panels.search", "Search"),
    ("status", "Status bar"),
    ("workspaces", "Workspaces"),
    ("history", "History"),
    ("chat", "Chat"),
    ("editor", "Editing"),
    ("agents", "Agents"),
    ("resources", "Resources"),
    ("tasks", "Tasks"),
    ("changes", "Changes"),
    ("panels.changes", "Changes panel"),
    ("assistant", "Mawas"),
    ("integrations", "Agent Integrations"),
    ("automation", "Automation"),
    ("launcher", "Start Sheet"),
    ("accounts", "Accounts"),
    ("models", "Models"),
    ("notifications", "Notifications"),
    ("sessions_restore", "Sessions & restore"),
    ("updates", "Updates"),
    ("control", "Agent control & security"),
    ("agents_tools", "Agents & tools"),
    ("experimental", "Experimental features"),
];

/// The pages of the sheet's list ([`super::settings_schema::PAGES`]): the title in the list
/// and the header, and what the page is for, one line under the title (13 pt muted; 80
/// characters at most so it stays one line in the 592 pt column). A page's sections keep the
/// title and the line of the page they used to be (`NAMESPACES`, `NAMESPACE_DESCRIPTIONS`).
const PAGE_TEXTS: &[(&str, &str, &str)] = &[
    (
        "appearance",
        "Appearance",
        "Themes and icons, density, docks and panels, and the status bar.",
    ),
    (
        "app",
        "General",
        "Starting and quitting, notifications, what comes back, and updates.",
    ),
    (
        "terminal",
        "Terminal",
        "Shell, fonts, scrollback and how terminals behave.",
    ),
    (
        "editor",
        "Editing & keys",
        "The code editor, keyboard shortcuts and Vim keys.",
    ),
    (
        "assistant",
        "Mawas",
        "Mawas, the assistant: models, memory and what it may do.",
    ),
    (
        "agents",
        "Agents & chat",
        "Agent chats and the Agents page, the Start sheet and requests that wait.",
    ),
    (
        "accounts",
        "Accounts & models",
        "Sign in to agents and set up the model providers Hutan can talk to.",
    ),
    (
        "integrations",
        "Integrations & hooks",
        "Hooks that connect agents to Hutan, and commands that run on events.",
    ),
    (
        "panels.sessions",
        "Projects & sessions",
        "How projects, workspaces, sessions and tasks are listed.",
    ),
    (
        "panels.files",
        "Files & changes",
        "Files, changes, search, problems and instructions.",
    ),
    (
        "history",
        "History",
        "Searchable history of your agent sessions.",
    ),
    (
        "data",
        "Data & logs",
        "Where Hutan keeps its files and logs, and how to open them.",
    ),
    (
        "agents_tools",
        "Tools & security",
        "Agent programs and MCP servers, what agents may do, and the process monitor.",
    ),
    (
        "experimental",
        "Experimental",
        "Features that ship switched off, at your own risk.",
    ),
];

/// What each section is for, one line under its title (13 pt muted; 80 characters at most so it
/// stays one line in the 592 pt column).
const NAMESPACE_DESCRIPTIONS: &[(&str, &str)] = &[
    (
        "appearance",
        "How Hutan looks: density, color mode, themes and icons.",
    ),
    (
        "app",
        "The daemon, the login item, notifications and what closing the window does.",
    ),
    (
        "data",
        "Where Hutan keeps its files and logs, and how to open them.",
    ),
    (
        "keymap",
        "Keyboard shortcuts, the keymap file and Vim keys.",
    ),
    (
        "terminal",
        "Shell, fonts, scrollback and how terminals behave.",
    ),
    ("layout", "Docks, panels and saved layouts."),
    ("tabs", "What closing a tab does to the session it shows."),
    (
        "panels.sessions",
        "How projects and sessions are listed in the sidebar.",
    ),
    ("sidebar", "How the sidebar groups and sorts sessions."),
    (
        "panels.inspector",
        "Approvals and questions that wait for you.",
    ),
    (
        "panels.files",
        "The Files panel: what it shows and how it behaves.",
    ),
    (
        "panels.problems",
        "Errors and warnings from language servers.",
    ),
    (
        "panels.instructions",
        "The Instructions panel and the files it lists.",
    ),
    (
        "panels.context",
        "The Context panel: how the agent's context is filled and its turns.",
    ),
    ("panels.search", "Search options and limits."),
    ("status", "What the status bar shows."),
    (
        "workspaces",
        "How projects are sorted and which ones the sidebar hides.",
    ),
    ("history", "Searchable history of your agent sessions."),
    ("chat", "Sending messages and attachments in agent chats."),
    (
        "editor",
        "The code editor: indentation, wrapping and saving.",
    ),
    (
        "agents",
        "The Agents page: what it lists and how it groups.",
    ),
    (
        "resources",
        "The process monitor: refresh rate and sorting.",
    ),
    ("tasks", "The Tasks panel and what it shows."),
    ("changes", "Reviewing changes: diffs, comments and merging."),
    (
        "panels.changes",
        "How the Changes panel and view list files.",
    ),
    (
        "assistant",
        "Mawas, the assistant: models, memory and what it may do.",
    ),
    (
        "integrations",
        "Connect Codex, Claude Code and OMP to Hutan.",
    ),
    ("automation", "Commands that run when something happens."),
    (
        "launcher",
        "The Start sheet: confirmations and safety for new agent sessions.",
    ),
    (
        "accounts",
        "Sign in to agent harnesses and see which accounts they use.",
    ),
    ("models", "Model providers and their credentials."),
    (
        "notifications",
        "When Hutan notifies you: requests, failures and finished work.",
    ),
    (
        "sessions_restore",
        "What comes back when the daemon restarts.",
    ),
    (
        "updates",
        "Whether Hutan checks for a newer version, and which releases count.",
    ),
    (
        "control",
        "What agents may ask Hutan to do, containment and finishing a task.",
    ),
    (
        "agents_tools",
        "Agent programs, MCP servers and checkpoints.",
    ),
    (
        "experimental",
        "Features that ship switched off, at your own risk.",
    ),
];

const FIELDS: &[FieldText] = &[
    // ---- appearance ----
    choice(
        "appearance",
        "density",
        "Density",
        "Compact fits more on screen; Comfortable gives rows more room.",
        &[("compact", "Compact"), ("comfortable", "Comfortable")],
    ),
    choice(
        "appearance",
        "appearance",
        "Color mode",
        "Light or dark windows. System follows macOS and switches with it.",
        &[("system", "System"), ("light", "Light"), ("dark", "Dark")],
    ),
    choice(
        "appearance",
        "reduce_motion",
        "Reduce motion",
        "Whether menus, sheets and toasts fade and slide. System follows macOS Accessibility. Reduced, nothing slides and fades stay under 150 ms.",
        &[("system", "System"), ("on", "On"), ("off", "Off")],
    ),
    choice(
        "appearance",
        "scrollbars",
        "Scroll bars",
        "When scroll bars show. System follows macOS (Show scroll bars). Always keeps a bar on every scrolled area; When scrolling shows an overlay bar while you scroll or point at the edge and fades it after a second.",
        &[
            ("system", "System"),
            ("always", "Always"),
            ("scrolling", "When scrolling"),
        ],
    ),
    field(
        "appearance",
        "light_theme",
        "Light theme",
        "The color theme of light windows. Moving through the list previews each theme; Return applies it. A missing theme falls back to Hutan Light.",
    ),
    field(
        "appearance",
        "dark_theme",
        "Dark theme",
        "The color theme of dark windows. Moving through the list previews each theme; Return applies it. A missing theme falls back to Hutan Dark.",
    ),
    field(
        "appearance",
        "icon_theme",
        "Icon theme",
        "The icons, chosen independently of the colors. Icons a theme leaves out fall back to Phosphor; Return applies the one you preview.",
    ),
    field(
        "appearance",
        "ui_font_family",
        "Interface font",
        "The font of the interface and of chat text, picked from the fonts installed on this Mac. Unset, the color theme's font is used; a theme that names its own prose font keeps it.",
    ),
    field(
        "appearance",
        "theme_overrides",
        "Theme color overrides",
        "Colors you change on top of a theme, by theme id. Edit the settings file to change these; the themes guide lists every color.",
    ),
    // ---- general ----
    field(
        "app",
        "keep_running",
        "Keep the daemon running after Hutan quits",
        "Quitting Hutan leaves its daemon and your sessions running, without asking. Off, the daemon stops when Hutan quits.",
    ),
    field(
        "app",
        "start_at_login",
        "Start the daemon when you log in",
        "Registers Hutan's launch agent in macOS Login Items: the daemon starts at login and restarts after a crash. Needs Hutan.app.",
    ),
    field(
        "app",
        "setup_done",
        "First run finished",
        "Whether the first run is over. Turn it off to see the first screen again at the next start; “Run Setup Again…” below runs the setup.",
    ),
    field(
        "app",
        "ask_notifications",
        "Offer notifications",
        "Offers once to notify you the first time something wants you while Hutan is in the background. Off after you answered or dismissed it.",
    ),
    field(
        "app",
        "ask_hooks",
        "Offer hooks for an agent in a terminal",
        "Offers once, under a terminal where Codex, Claude Code or OMP runs unreported, to install its hooks. Nothing is written unless you accept.",
    ),
    choice(
        "app",
        "on_last_window_close",
        "When the last window closes",
        "Quit Hutan (it asks first while sessions run), or keep it in the menu bar without a Dock icon; that needs the menu bar item below.",
        &[("quit", "Quit Hutan"), ("menu_bar", "Keep in the menu bar")],
    ),
    field(
        "app",
        "menu_bar",
        "Show the menu bar item",
        "A menu bar icon with the daemon's state, the sessions waiting for you, and a menu to jump to them. With no window the daemon has its own.",
    ),
    field(
        "notifications",
        "dock_badge",
        "Show a badge on the Dock icon",
        "Shows how many sessions have a request waiting (an approval, a question, a plan) on the Dock icon. Failures and finished work never count.",
    ),
    field(
        "notifications",
        "usage_warnings",
        "Warn when a usage limit runs low",
        "Shows a toast when one of a harness's limit windows (the 5-hour or the weekly one) passes 80% and again at 95%, once per window. A limit that was already high when Hutan opened is not announced; the status item and the Agents page show it.",
    ),
    // ---- experimental ----
    field(
        "experimental",
        "remote_hosts",
        "Remote hosts over SSH (experimental)",
        "Lets the command line reach the Hutan of another Mac over SSH (hutan --host and hutan host). Off, both refuse to run.",
    ),
    // ---- keyboard ----
    choice(
        "keymap",
        "base",
        "Key bindings",
        "The shortcuts Hutan starts from: Zed's on macOS, or Hutan's own. Your keymap.toml is applied on top; a change takes effect at once.",
        &[("zed", "Zed keys"), ("hutan", "Hutan keys")],
    ),
    // ---- terminal ----
    field(
        "terminal",
        "font_family",
        "Font",
        "The font of terminal text, picked from the fonts installed on this Mac (monospaced ones unless you show all). Unset, the color theme's monospace font is used.",
    ),
    field(
        "terminal",
        "font_size",
        "Font size",
        "Terminal text size in points.",
    ),
    field(
        "terminal",
        "line_height",
        "Line height",
        "Space between terminal lines, as a multiple of the font size.",
    ),
    choice(
        "terminal",
        "theme",
        "Color theme",
        "The terminal's colors. Automatic follows the color theme in effect; an installed theme keeps its colors whatever the window does.",
        &[
            ("auto", "Automatic"),
            ("hutan-dark", "Hutan Dark"),
            ("hutan-light", "Hutan Light"),
        ],
    ),
    choice(
        "terminal",
        "cursor.style",
        "Cursor shape",
        "The shape of the cursor. A program that asks for another shape gets it.",
        &[
            ("block", "Block"),
            ("bar", "Bar"),
            ("underline", "Underline"),
        ],
    ),
    field(
        "terminal",
        "cursor.blink",
        "Blinking cursor",
        "Whether the cursor blinks while the terminal has keyboard focus.",
    ),
    choice(
        "terminal",
        "option_as_meta",
        "Option key as Meta",
        "Makes Option send Meta (Alt) for shell and editor shortcuts instead of special characters. Every choice except None applies to both keys.",
        &[
            ("none", "None"),
            ("left", "Left Option"),
            ("right", "Right Option"),
            ("both", "Both Option keys"),
        ],
    ),
    field(
        "terminal",
        "copy_on_select",
        "Copy on select",
        "Copies selected text to the clipboard as soon as you release the mouse.",
    ),
    field(
        "terminal",
        "scroll_to_bottom_on_input",
        "Jump to the latest output when typing",
        "When you type while scrolled back into history, return to the live screen.",
    ),
    choice(
        "terminal",
        "tab_close",
        "Closing a terminal tab (older setting)",
        "The older name of Tabs → Close the session with its tab, still read while that one is not set: Detach is Never, Close session is When idle.",
        &[
            ("detach", "Detach (keep the session)"),
            ("close_session", "Close the session"),
        ],
    ),
    choice(
        "terminal",
        "open_files_with",
        "Open files from terminal output with",
        "What ⌘-click on a file path in terminal output (src/lib.rs:42:7) opens. Editor (the default) opens it in a Hutan file tab at that line; System uses the file's default application. A URL always opens in the browser.",
        &[("editor", "Editor"), ("system", "System default app")],
    ),
    field(
        "terminal",
        "confirm_multiline_paste",
        "Confirm multi-line pastes",
        "Ask before a paste of several lines into a program that did not turn on bracketed paste, because a shell would run every line at once. Choose Paste, Paste as one line or Cancel. Programs that handle bracketed paste (most editors and agents) are never asked about.",
    ),
    field(
        "terminal",
        "paste_images",
        "Paste images as files",
        "⌘V of an image (a screenshot, Copy Image in a browser) into a terminal saves it as a PNG in that session's scratch folder, up to 20 MiB, and pastes the file's path, which Claude Code, Codex and OMP read as an image. A notice says where it went, with Reveal; the file goes when the session is closed. Off, an image is refused with a notice. Text on the clipboard always wins.",
    ),
    field(
        "terminal",
        "dim_unfocused_splits",
        "Dim unfocused splits",
        "Splits of a terminal tab that do not have the keyboard lose a few percent of contrast. Off, only the edge marks the focused one.",
    ),
    field(
        "terminal",
        "quick_terminal",
        "Quick terminal",
        "A terminal that drops down from the top of the screen on the shortcut below, from any app and over full-screen apps. It is one terminal session that Hutan keeps between shows: hiding it never ends the session. Escape or the shortcut hides it, ⌘T adds a tab inside it and ⌘⇧Return moves the tab into the main window. Off, the shortcut is released.",
    ),
    field(
        "terminal",
        "quick_hotkey",
        "Quick terminal shortcut",
        "The system-wide shortcut of the quick terminal, for example ctrl-shift-grave (the default), alt-space or cmd-shift-k. It needs at least one of cmd, ctrl or alt. Leave it empty to turn it off. If another app already uses it, Hutan tells you once and leaves the setting alone. It never shows the panel while another app is reading a password (secure input).",
    ),
    field(
        "terminal",
        "quick_height",
        "Quick terminal height",
        "How much of the screen's height the quick terminal covers, in percent. It opens on the screen the pointer is on.",
    ),
    choice(
        "terminal",
        "quick_start_in",
        "Quick terminal starts in",
        "Where the quick terminal's first session starts: your Home folder, or the folder of the project that is active in the main window when the panel first shows. Tabs added with ⌘T start next to the tab you are in.",
        &[("home", "Home"), ("project", "The active project")],
    ),
    field(
        "terminal",
        "quick_escape_hides",
        "Escape hides the quick terminal",
        "Escape hides the panel. A program that uses the alternate screen (vim, less, most TUIs) always gets its Escape. Turn this off if you run agents or editors that need Escape in the shell itself: the shortcut still hides the panel.",
    ),
    field(
        "terminal",
        "blocks",
        "Command blocks",
        "Groups each command and its output into a block with a marker bar, a status chip and hover actions. Off, a plain terminal.",
    ),
    field(
        "terminal",
        "shell",
        "Shell",
        "The shell new terminals run: a full path or a name found on your PATH. Leave it empty to use your account's login shell.",
    ),
    field(
        "terminal",
        "env",
        "Extra environment variables",
        "Variables added to the environment of new terminals, on top of your login environment. Terminals already open keep theirs.",
    ),
    // ---- layout ----
    choice(
        "layout",
        "preset",
        "Layout preset",
        "The layout preset applied when Hutan starts and whenever you change this. Pick one of the built-in presets or one you saved.",
        &[
            ("default", "Default"),
            ("focus", "Focus"),
            ("overview", "Agents overview"),
            ("explorer-left", "Explorer left"),
        ],
    ),
    field(
        "layout",
        "auto_collapse",
        "Collapse sidebars in a narrow window",
        "Shrinks the right panel to its rail when the window is narrower than 1180 pt and the left sidebar to its icon rail below 900 pt, and brings them back when the window grows. This is temporary: it never changes the layout you chose, and a sidebar you hid yourself stays hidden.",
    ),
    field(
        "layout",
        "rail_peek",
        "Open side panels over a narrow window",
        "In a window too narrow for the center to keep 480 pt beside a side panel, clicking a rail icon (or ⌘L, ⌘⇧E, a panel command) shows the panel floating over the edge of the center instead of squeezing it. Esc, a click outside, opening a file or the same icon closes it. Off: the panel is pushed in beside the center.",
    ),
    choice(
        "layout",
        "right_rail",
        "Right side when its panel is closed",
        "What closing the right panel (⌘L, the title bar button, a click on the open panel's icon) leaves. Like the left sidebar: the icon rail stays and the title bar button hides or shows the whole right side. Rail always: the icon rail never goes away. Hide it too: closing the panel hides the rail as well. A narrow window still shows the rail, so a panel can open over the center.",
        &[
            ("auto", "Like the left sidebar"),
            ("always", "Keep the icon rail"),
            ("never", "Hide it too"),
        ],
    ),
    field(
        "layout",
        "offer_new_default",
        "Offer the new default layout",
        "Shows a one-line offer once when your saved layout predates the quieter default (Changes, Agents and History on the right rail, Requests and Resources under ⋯): “Use new default” or “Keep mine”. Your layout stays as it is unless you choose it. Off after you answered.",
    ),
    field(
        "layout",
        "files_hint",
        "Point at Files on the right rail",
        "Shows a small label beside the Files icon of the right rail once, while the rail is all there is of the right side (a narrow window or a collapsed panel). Off after you used it or Files was shown.",
    ),
    field(
        "layout",
        "presets",
        "Your layout presets",
        "Layouts you saved, by name: save the one on screen, rename a preset or delete one. A preset named like a built-in one replaces it.",
    ),
    // ---- tabs ----
    choice(
        "tabs",
        "close_session",
        "Close the session with its tab",
        "What closing the tab of a terminal, a terminal agent or a chat does to its session (the × of the sidebar always closes it). Always closes the session and stops what runs in it, without asking. When idle closes it when nothing runs, and asks when something does: Keep running in background closes the tab only, Close stops it. Never closes the tab only; the session keeps running in the sidebar. Closed sessions show an Undo, and History can reopen them.",
        &[
            ("always", "Always"),
            ("when_idle", "When nothing runs"),
            ("never", "Never (keep it in the sidebar)"),
        ],
    ),
    // ---- projects sidebar ----
    choice(
        "panels.sessions",
        "group_by",
        "Group sessions by",
        "How the sidebar groups sessions.",
        &[
            ("workspace", "Workspace"),
            ("state", "State"),
            ("none", "Not grouped"),
        ],
    ),
    field(
        "panels.sessions",
        "needs_you",
        "Requests, Failed and Ready for review groups",
        "Shows the groups on top of the sidebar: Requests (an approval, a question or a plan waits), Failed, and Ready for review.",
    ),
    choice(
        "panels.sessions",
        "row",
        "Row contents",
        "What each row shows, in order. State and Kind are one lane: the harness glyph with the state's badge on it. A coding agent's row has a second line only when there is news (a question, a failure, the tool it runs, an unread reply); folder (where the session is now, once it has left its checkout) and placement (a worktree branch) are muted text after the title. Pick the parts from the menu.",
        &[
            ("state", "State"),
            ("kind", "Kind"),
            ("title", "Title"),
            ("folder", "Folder"),
            ("placement", "Placement"),
            ("age", "Age"),
        ],
    ),
    field(
        "panels.sessions",
        "show_diffstat",
        "Show added and removed line counts",
        "Off by default. On adds +N −M (lines added and removed since the last commit) to the project and worktree rows that have changes.",
    ),
    field(
        "panels.sessions",
        "hooks_hint",
        "Point at agent hooks on the first agent row",
        "Shows a small ⓘ once, on the first coding agent in a terminal that no hook speaks for: it says what installing the agent's hooks adds (its model and the tool it runs on its card) and opens Agent integrations. Off after you used it.",
    ),
    choice(
        "sidebar",
        "organize",
        "Organize the sidebar",
        "Workspaces first lists workspaces with their projects, then loose projects, then Home. Projects only is one flat list, pinned first.",
        &[
            ("workspaces", "Workspaces, then projects"),
            ("projects", "Projects only"),
        ],
    ),
    field(
        "sidebar",
        "show_all_sessions",
        "Show every container's sessions",
        "Off, the sidebar lists the container you work in with its sessions under it, and the others collapsed with their counts. On, every project and workspace is expanded.",
    ),
    choice(
        "sidebar",
        "nav_rows",
        "Navigation rows",
        "The rows over the sidebar's list, in order: Agents, Mawas, Memory and Tasks. A row left out is hidden (the row of the page you are on shows anyway). The sidebar's right-click menu hides and moves them too.",
        &[
            ("agents", "Agents"),
            ("assistant", "Mawas"),
            ("memory", "Memory"),
            ("tasks", "Tasks"),
        ],
    ),
    field(
        "sidebar",
        "show_projects",
        "Show the projects list",
        "Shows the list of projects, workspaces and sessions under the navigation rows. Off, the sidebar keeps the rows and the bottom buttons; the right-click menu brings the list back.",
    ),
    // ---- inspector ----
    choice(
        "panels.inspector",
        "tabs",
        "Tabs",
        "The inspector's tabs, in order: requests, changes and info. Pick the tabs from the menu.",
        &[
            ("approvals", "Requests"),
            ("changes", "Changes"),
            ("info", "Info"),
        ],
    ),
    field(
        "panels.inspector",
        "reveal_on_approval",
        "Open when an approval arrives",
        "Shows the inspector as soon as an agent asks for an approval.",
    ),
    // ---- files ----
    field(
        "panels.files",
        "show_ignored",
        "Show ignored files",
        "Lists the files and folders that .gitignore or .ignore rules leave out, dimmed. Off, the tree hides them.",
    ),
    field(
        "panels.files",
        "show_hidden",
        "Show hidden files",
        "Lists files and folders whose names start with a dot (.github, .env). Off, the tree hides them.",
    ),
    field(
        "panels.files",
        "compact_folders",
        "Fold single-folder chains",
        "Shows a folder that holds only one folder on the same row as it (src/utils/helpers), opened and closed as one.",
    ),
    choice(
        "panels.files",
        "indent_guides",
        "Indent guides",
        "The vertical lines along open folders: only while the pointer is over the tree, always, or never.",
        &[("hover", "On hover"), ("always", "Always"), ("off", "Off")],
    ),
    field(
        "panels.files",
        "auto_reveal",
        "Reveal the active file",
        "Selects the file of the tab you switch to in the Files panel, opening the folders it is in.",
    ),
    field(
        "panels.files",
        "confirm_delete",
        "Confirm before moving to the Trash",
        "Asks first, listing what goes, the tabs with unsaved changes and an agent working in the folder. Files only ever go to the Trash, never straight to deletion.",
    ),
    field(
        "panels.files",
        "confirm_drag",
        "Confirm the first drag to a folder",
        "Asks the first time in a session that you drop a row onto another folder, and not again after you confirm.",
    ),
    choice(
        "panels.files",
        "filter_mode",
        "Filter mode",
        "What typing in the Files panel's filter does: show only the files that match and the folders they are in, or keep the tree as it is and mark the matches.",
        &[("filter", "Filter"), ("highlight", "Highlight")],
    ),
    choice(
        "panels.files",
        "show_diagnostics",
        "Diagnostics in the tree",
        "Marks files the language servers found errors or warnings in with a count and a red or amber name, and the folders that hold them. Off, the panel does not ask for them.",
        &[
            ("errors_warnings", "Errors and warnings"),
            ("errors", "Errors only"),
            ("off", "Off"),
        ],
    ),
    choice(
        "panels.files",
        "open_editors",
        "Open Editors section",
        "Lists the files open in tabs above the tree: Auto shows it while two or more files of the folder are open.",
        &[("auto", "Auto"), ("always", "Always"), ("never", "Never")],
    ),
    choice(
        "panels.files",
        "roots",
        "Folders shown",
        "Active project shows the folder of the project the window works in. Whole workspace stacks every project of the active workspace as a collapsible root row in one tree; the filter and the sections look across all of them. The scope chip's menu picks another checkout or project for a while.",
        &[
            ("active", "Active project"),
            ("workspace", "Whole workspace"),
        ],
    ),
    // ---- problems ----
    choice(
        "panels.problems",
        "severity",
        "Show",
        "The least serious diagnostics the Problems panel lists; the panel's header switches it too.",
        &[
            ("all", "Everything"),
            ("warning", "Errors and warnings"),
            ("error", "Errors only"),
        ],
    ),
    // ---- instructions ----
    field(
        "panels.instructions",
        "show_user_files",
        "List my own instruction files",
        "Lists ~/.codex/AGENTS.md, ~/.claude/CLAUDE.md and ~/.omp/agent/AGENTS.md under the project's files, read-only. Off, the panel never looks at them.",
    ),
    // ---- context ----
    field(
        "panels.context",
        "show_fill_chart",
        "Show the context after each turn",
        "Draws a small bar chart of how full the context was after each turn above the turn list.",
    ),
    field(
        "panels.context",
        "show_sources",
        "Say where the numbers come from",
        "Lists at the end of the panel which block came from the chat's own report and which from the harness's /context.",
    ),
    // ---- status bar ----
    field(
        "status",
        "workspace.show_branch",
        "Show the branch",
        "Shows the branch checked out in the project after its name in the status bar. The item's right-click menu switches it too.",
    ),
    field(
        "status",
        "connection.show_version",
        "Show the daemon's version",
        "Says which version of the daemon is running (hutand 0.2.0) in the status bar while it is up. The item's right-click menu switches it too.",
    ),
    choice(
        "status",
        "usage.style",
        "Usage limits",
        "How the limits your harnesses report show in the status bar, once one has (nothing is drawn before). Rings: one small ring per harness with the share of its tightest window, amber from 80%, red from 95%; hover it for every window and when it resets, click it for the Agents page. Words: the tightest limit as text, such as Codex 72% · resets 3h. The Agents page's Usage section has the numbers either way.",
        &[("meters", "Rings"), ("text", "Words"), ("off", "Not shown")],
    ),
    // ---- search ----
    field(
        "panels.search",
        "case_sensitive",
        "Match case",
        "Finds only text with the same case. Off, a query with an upper-case letter still matches case exactly. The Aa button switches it too.",
    ),
    field(
        "panels.search",
        "whole_word",
        "Whole words",
        "Finds the query only as a whole word, not inside a longer one. The panel's ab button switches it too.",
    ),
    field(
        "panels.search",
        "regex",
        "Regular expression",
        "Reads the query as a regular expression; $1 in the replacement inserts the first group. The panel's .* button switches it too.",
    ),
    field(
        "panels.search",
        "max_results",
        "Most results",
        "How many matching lines one search lists, up to 1 000. The panel says when a search stopped there.",
    ),
    field(
        "panels.search",
        "debounce_ms",
        "Wait after typing (ms)",
        "How long typing must pause before the search runs; every change stops the search before it.",
    ),
    // ---- workspaces ----
    choice(
        "workspaces",
        "sort",
        "Sort by",
        "How projects and workspaces are ordered in the sidebar, within each level.",
        &[
            ("manual", "Manual order"),
            ("recent", "Most recent"),
            ("attention", "Needs attention first"),
            ("name", "Name"),
        ],
    ),
    field(
        "workspaces",
        "hide_empty",
        "Hide projects without sessions",
        "Leaves out projects that have no session at all.",
    ),
    field(
        "workspaces",
        "hide_sleeping",
        "Hide projects without a running session",
        "Leaves out projects that have no session running right now.",
    ),
    field(
        "workspaces",
        "hide_cli_created",
        "Hide projects made by the command line",
        "Leaves out projects that the hutan command or an agent created.",
    ),
    field(
        "workspaces",
        "always_show_active",
        "Always show where you are",
        "Keeps the project and workspace you are in on the list even when a filter above would hide them.",
    ),
    // ---- history ----
    field(
        "history",
        "sources",
        "Search these sources by default",
        "Which history sources the search starts with: hutan, claude, codex or omp. Leave empty to search all of them.",
    ),
    field(
        "history",
        "roles",
        "Search these kinds of text by default",
        "Which kinds of text the search starts with: user, assistant, tool, command or output. Leave empty to search all of them.",
    ),
    choice(
        "history",
        "range",
        "How far back to search",
        "The time range the search starts with.",
        &[
            ("24h", "Last 24 hours"),
            ("7d", "Last 7 days"),
            ("30d", "Last 30 days"),
            ("all", "All time"),
        ],
    ),
    choice(
        "history",
        "project",
        "Search in",
        "Whether the search covers the workspace you are in or every project.",
        &[
            ("workspace", "The current workspace"),
            ("all", "All projects"),
        ],
    ),
    field(
        "history",
        "show_snippets",
        "Show matching text",
        "Shows the matching text under each search result.",
    ),
    // ---- chat ----
    choice(
        "chat",
        "send_key",
        "Send messages with",
        "Which key sends a message, in every place you write one: chats and Mawas, the Start sheet's prompt, review comments and free-text answers to an agent's questions. ⌘Return always sends too. Merge and delivery messages, memory notes and settings fields keep their own keys.",
        &[
            ("cmd-enter", "⌘Return (Return adds a line)"),
            ("enter", "Return (⇧Return adds a line)"),
        ],
    ),
    field(
        "chat",
        "ask_send_key",
        "Ask which key sends",
        "Shows a one-line question about the send key once, above the first message box you use. Off after you answer or dismiss it.",
    ),
    field(
        "chat",
        "attach_max_bytes",
        "Largest block to attach (bytes)",
        "The most output of a terminal block that “Send to agent” attaches to a message. Longer output is cut, and the attachment says so.",
    ),
    choice(
        "chat",
        "fork_where",
        "Fork a chat into",
        "Where the Fork popover starts: the chat's own folder, or a new worktree whose files are the turn's. The popover remembers your last choice here.",
        &[
            ("this_folder", "This folder"),
            ("new_worktree", "A new worktree"),
        ],
    ),
    // ---- editing ----
    field(
        "editor",
        "vim_mode",
        "Vim mode",
        "Vim-style editing (Normal, Insert and Visual modes) in file tabs and Hutan's text fields. File tabs open in Normal mode. Fields start in Insert mode; Escape switches to Normal, and Escape again does what it always did, such as closing a dialog. Terminals are not affected.",
    ),
    field(
        "editor",
        "vim_use_system_clipboard",
        "Vim uses the system clipboard",
        "On, as in Zed: copies and deletes in Vim mode also go to the system clipboard. Off, they use Vim's own registers, as Vim does.",
    ),
    field(
        "editor",
        "vim_mappings.insert",
        "Vim mappings in Insert mode",
        "Your own keys for Insert mode, as Vim's inoremap, such as jk for <Esc>. Edit them in settings.toml; the Keyboard section of Editing & keys lists them.",
    ),
    field(
        "editor",
        "vim_mappings.normal",
        "Vim mappings in Normal mode",
        "Your own keys for Normal mode, as Vim's nnoremap, such as Y for y$. Edit them in settings.toml.",
    ),
    field(
        "editor",
        "vim_mappings.visual",
        "Vim mappings in Visual mode",
        "Your own keys for the Visual modes, as Vim's xnoremap. Edit them in settings.toml.",
    ),
    field(
        "editor",
        "vim_timeout_ms",
        "Vim mapping timeout (ms)",
        "How long a mapping of several keys waits for the next one, as Vim's timeoutlen. After that the keys typed count as themselves.",
    ),
    field(
        "editor",
        "font_family",
        "Code font",
        "The font of code in file tabs, diffs and chat, picked from the fonts installed on this Mac (monospaced ones unless you show all). Unset, the color theme's monospace font is used.",
    ),
    field(
        "editor",
        "font_size",
        "Code font size",
        "Font size of code in file tabs, in points.",
    ),
    field(
        "editor",
        "line_height",
        "Code line height",
        "Line height of code in file tabs, as a multiple of the font size.",
    ),
    field(
        "editor",
        "large_file_bytes",
        "Large file threshold (bytes)",
        "Files larger than this open only after you confirm, read-only and without syntax highlighting.",
    ),
    choice(
        "editor",
        "autosave",
        "Save files automatically",
        "Off: only ⌘S saves. On focus change: a file tab saves when it loses the keyboard or the window is left. After a delay: it saves a moment after you stop typing. A file that changed on disk meanwhile is never saved by itself; the conflict bar asks.",
        &[
            ("off", "Off"),
            ("on_focus_change", "On focus change"),
            ("after_delay", "After a delay"),
        ],
    ),
    field(
        "editor",
        "autosave_delay_ms",
        "Autosave delay (ms)",
        "With Save files automatically set to After a delay: how long after the last change a file tab saves.",
    ),
    field(
        "editor",
        "agent_highlight",
        "Highlight agents' changes",
        "File tabs tint the lines an agent's turn changed and let you reject one change at a time. Off, nothing is compared or drawn.",
    ),
    field(
        "editor",
        "git_gutter",
        "Show git changes in the gutter",
        "A bar beside the line numbers marks the lines you added or changed against the index (or HEAD when nothing of the file is staged), and a wedge where lines were removed. Click one to see what it was, revert it or stage it. Off, the file is not compared with git.",
    ),
    field(
        "editor",
        "preview_tabs",
        "Preview tabs",
        "Every file you open keeps its own tab. On, a single click on a file in the Files tree shows it in a preview tab (its title is in italics) that the next file you click replaces; a double click, an edit or Keep Open in the tab's menu makes it a regular tab.",
    ),
    field(
        "editor",
        "open_beside_agent",
        "Open files beside the agent",
        "While an agent (a chat or a terminal agent) is in front, a file you open — from Files, ⌘P, a link in the chat — opens in a split to its right instead of replacing it; the split is reused for the next file. Off, a file opens in the pane you are in. ⌥-click, ⌥Return and Open to the Side always open to the side.",
    ),
    choice(
        "editor",
        "markdown_preview",
        "Markdown preview",
        "What a Markdown file offers for showing its rendered page. Toggle: ⇧⌘V (and the Source | Preview switch above the file) turns the tab into the page and back. Beside the file: ⇧⌘V opens the page in a split to the right that follows your edits and scrolling. Off: no preview. Markdown: Open Preview to the Side in the command palette always opens the split unless this is Off. No language server is involved.",
        &[
            ("off", "Off"),
            ("toggle", "Toggle in the tab"),
            ("side", "Beside the file"),
        ],
    ),
    choice(
        "editor",
        "markdown_open",
        "Open Markdown files as",
        "Source shows the text, as for every file. Preview shows the rendered page, with Source one click (or ⇧⌘V) away. Only when Markdown preview is not Off.",
        &[("source", "Source"), ("preview", "Preview")],
    ),
    field(
        "editor",
        "scroll_beyond_last_line",
        "Scroll past the last line",
        "How far a file scrolls past its last line, as a fraction of the window's height: 0 stops with the last line at the bottom, 0.5 leaves half a screen of room, 1 a whole one. A file is never scrolled to a blank view.",
    ),
    field(
        "editor",
        "inline_blame",
        "Show git blame at the caret",
        "The caret's line shows who last changed it, when, and the commit message after the end of the line. Off by default; Toggle Blame in the command palette shows a column for every line.",
    ),
    choice(
        "editor",
        "soft_wrap",
        "Wrap long lines",
        "Auto wraps Markdown and plain text at the width of the window and lets code scroll sideways. Window width and At a column wrap every file; Off never does. A language of your own can say otherwise in settings.toml, under languages. View, Toggle Soft Wrap (⌥Z) flips it for one tab.",
        &[
            ("auto", "Auto"),
            ("off", "Off"),
            ("viewport", "Window width"),
            ("column", "At a column"),
        ],
    ),
    field(
        "editor",
        "wrap_column",
        "Wrap column",
        "With Wrap long lines set to At a column: how wide a row is, in cells (20 to 1000). A window narrower than that wraps at its own edge.",
    ),
    // ---- agents ----
    field(
        "agents",
        "show_finished",
        "Show finished subagents",
        "In the Agents panel, lists subagents that have finished, dimmed. Off, only the ones still around are listed.",
    ),
    field(
        "agents",
        "only_blocked",
        "Only blocked subagents",
        "In the Agents panel, lists only the subagents that are blocked, plus the ones above them.",
    ),
    choice(
        "agents",
        "scope",
        "Panel shows agents from",
        "Which agent sessions the Agents panel lists: those of the current workspace, only the session you are looking at, or every workspace.",
        &[
            ("active_workspace", "The current workspace"),
            ("session", "The focused session"),
            ("all", "All workspaces"),
        ],
    ),
    field(
        "agents",
        "page_group_by_workspace",
        "Group the Agents page by workspace",
        "On the Agents page, lists each workspace under its own heading, with the same state groups inside. Off, the groups are by state alone.",
    ),
    field(
        "agents",
        "page_show_finished",
        "Show finished agents on the page",
        "On the Agents page, lists the sessions that finished. Off, they are folded under a Finished heading.",
    ),
    field(
        "agents",
        "page_usage",
        "Show usage on the Agents page",
        "On the Agents page, a Usage section lists each harness's limit windows (5-hour, weekly) with their reset times, and the tokens and cost of the turns Hutan ran today and this week. A cost is the harness's own estimate.",
    ),
    // ---- resources ----
    field(
        "resources",
        "interval_ms",
        "Refresh interval (milliseconds)",
        "How often the daemon measures processes while the Resources panel is open. Nothing is measured while the panel is hidden.",
    ),
    choice(
        "resources",
        "sort",
        "Sort by",
        "How sessions, and the processes inside each, are ordered. The column headers change it too.",
        &[("cpu", "CPU"), ("memory", "Memory"), ("name", "Name")],
    ),
    // ---- tasks ----
    field(
        "tasks",
        "show_done",
        "Show finished tasks",
        "Lists tasks that are done or cancelled. Off, they are hidden and the tasks that waited for them move up; failed tasks always stay.",
    ),
    choice(
        "tasks",
        "scope",
        "Show tasks from",
        "Whether the Tasks panel lists tasks of the current workspace or of every workspace.",
        &[
            ("workspace", "The current workspace"),
            ("all", "All workspaces"),
        ],
    ),
    // ---- changes ----
    choice(
        "changes",
        "default_scope",
        "Changes panel lists",
        "What the Changes panel shows when it opens: the files of the checkout you work in, or every checkout of the workspace grouped by checkout. It only applies while the workspace has more than one repository or a worktree with changes; the panel's own switch changes it for this window.",
        &[
            ("checkout", "This checkout"),
            ("workspace", "All in workspace"),
        ],
    ),
    choice(
        "changes",
        "wrap_lines",
        "Wrap long lines in diffs",
        "Whether the Changes view's diff wraps long lines instead of scrolling sideways. Automatic wraps Markdown and text files, where lines are sentences, and scrolls code sideways. The Wrap lines toggle in the diff header changes it for the open view.",
        &[
            ("auto", "Automatic (text wraps, code scrolls)"),
            ("on", "Always"),
            ("off", "Never"),
        ],
    ),
    choice(
        "changes",
        "list",
        "Changes view lists files",
        "One row per file with its name first, or grouped by folder with folders you can fold. The list's own toggle changes it for the open view.",
        &[("flat", "Flat"), ("tree", "As a tree")],
    ),
    field(
        "panels.changes",
        "show_diffstat",
        "Show added and removed line counts",
        "Off by default: the Changes panel and view list the files and their status. On adds +N −M beside each file and on the checkout's summary.",
    ),
    // ---- assistant ----
    field(
        "assistant",
        "enabled",
        "Use Mawas",
        "On (the default) adds Mawas to the left sidebar's list; it opens as a page in the center. Off hides Mawas from the left sidebar, removes its commands and shortcuts, and releases the system-wide shortcut. Your earlier Mawas conversations are kept.",
    ),
    field(
        "assistant",
        "global_shortcut",
        "System-wide shortcut",
        "A shortcut that brings Hutan forward and opens Mawas from any app, for example alt-space or cmd-shift-space. It needs at least one of cmd, ctrl or alt. Leave it empty to turn it off. If another app already uses it, Hutan tells you once and leaves the setting alone.",
    ),
    field(
        "assistant",
        "chat_width_rem",
        "Chat width",
        "How wide Mawas's conversation is, in rem (30 to 90; 46 by default). Drag either edge of the column to set it, or use Mawas: Wider Chat, Narrower Chat and Reset Chat Width. A page narrower than the width keeps a small margin on each side.",
    ),
    field(
        "assistant",
        "rail_open",
        "Show Today",
        "Shows the Today column beside the conversation: what waits for you, failed and finished work, and what is running. Hiding it never loses a request: the button in Mawas's header counts the sessions that wait for an answer. A page below 640 pt has no room for the column either way (Today is a page of its own in the ⋯ menu).",
    ),
    field(
        "assistant",
        "rail_width_rem",
        "Today width",
        "How wide the Today column is, in rem (17.5 to 32.5; 21.25 by default). Drag the column's left border to set it; a double click puts it back. A page below 640 pt has no column.",
    ),
    choice(
        "assistant",
        "rail_tab",
        "Today tab",
        "The tab of the Today column that is open: Now (the board of every agent: requests, failed, ready for review, working, idle), Recent (what happened since you last looked: turns, requests and how long they waited, files changed, tasks, sessions) or Memory (what waits for you in Mawas's memory and its latest entries). The column opens on the one you left.",
        &[("now", "Now"), ("recent", "Recent"), ("memory", "Memory")],
    ),
    choice(
        "assistant",
        "home_scope",
        "What Mawas Home opens",
        "Global (the default) opens the Global Mawas, which looks at every workspace and agent; a workspace is a Focus you pick in the header, a filter that adds that workspace's notes and project line, never another assistant or another permission. Follow the workspace keeps how Home worked before: it shows the active workspace's own Mawas. Your earlier workspace conversations and memory are kept either way, under Chats in the header.",
        &[
            ("global", "The Global Mawas"),
            ("follow_workspace", "Follow the active workspace"),
        ],
    ),
    // ---- agent integrations ----
    field(
        "integrations",
        "auto_install",
        "Keep agent hooks installed on every start",
        "Off by default: Hutan never writes into an agent's own configuration unless you asked. When on, each time Hutan starts it adds its hooks to Codex, Claude Code and OMP, for each one that is installed and not yet connected (~/.codex, ~/.claude, ~/.omp/agent, even with a custom HUTAN_HOME). Only Hutan's own entries are written, and the files are backed up first. The hooks do nothing outside a Hutan session. Turning this off stops future installs but removes nothing: use Uninstall hooks below to remove them.",
    ),
    // ---- the Start sheet ----
    field(
        "launcher",
        "confirm_agent_start",
        "Ask before an agent chat sends its prompt",
        "The first time you start an agent chat with a prompt on a given agent and model, the Start sheet asks before it sends the prompt (it spends model usage). “Don't ask again for this model” remembers the choice per model. Turn this off to start at once. Plain terminals, commands and queued chats never ask. The choices you remembered are listed below, where you can revoke them.",
    ),
    choice(
        "launcher",
        "safety",
        "Safety the Start sheet opens on",
        "What the Safety row of the Start sheet starts on for agent chats and terminal agents: Off leaves the agent's own approval and sandbox settings alone; Ask, Contained (no questions inside a boundary: the agent's own sandbox, or Hutan's Seatbelt for OMP) and Read-only choose a preset. An agent that cannot honour it starts on Off and the sheet says why. Full access is never a default. Put the key in a project's .hutan/settings.toml to set it for that project.",
        &[
            ("off", "Off (the agent's own settings)"),
            ("ask", "Ask"),
            ("contained", "Contained"),
            ("read_only", "Read-only"),
        ],
    ),
];

fn entry(namespace: &str, key: &str) -> Option<&'static FieldText> {
    FIELDS
        .iter()
        .find(|f| f.namespace == namespace && f.key == key)
}

/// Daemon settings (`daemon.toml`): the wording of every key `daemon.settings.get` answers.
/// Kept apart from [`FIELDS`] because these keys are the daemon's, not a namespace of the
/// settings store; a test checks that each key of the daemon's table has an entry.
const DAEMON_FIELDS: &[FieldText] = &[
    // ---- updates ----
    choice(
        "daemon",
        "updates.check",
        "Check for updates",
        "Once a day at most, Hutan asks GitHub's release list (api.github.com) whether a newer version exists. The request carries only a User-Agent header with this version; nothing about you or your work is sent. Only on request, it asks when you choose Check for Updates… Nothing ever updates by itself.",
        &[("auto", "Automatically"), ("off", "Only when I ask")],
    ),
    choice(
        "daemon",
        "updates.channel",
        "Releases that count",
        "Which releases are offered as updates. Every Hutan release is a pre-release for now, so Stable offers nothing until one is not.",
        &[
            ("prerelease", "All releases, pre-releases too"),
            ("stable", "Stable releases only"),
        ],
    ),
    field(
        "daemon",
        "updates.skip_version",
        "Skipped version",
        "The version you chose to skip: Hutan does not announce it in the status bar. A newer version is announced again. Empty skips nothing.",
    ),
    // ---- notifications ----
    field(
        "daemon",
        "notify.enabled",
        "Notifications",
        "Raise a notification when an agent asks you something, fails or finishes something. Off, nothing is raised, in Hutan or in the notification centre.",
    ),
    field(
        "daemon",
        "notify.native",
        "Show in the macOS notification centre",
        "Also post notifications to the system. Only the Hutan app can do this; the daemon of a development build posts nothing. A request or a failure notifies when Hutan is behind other windows or the session is not the one on screen; a request that arrives for another session while Hutan is in front also shows a toast in the window.",
    ),
    choice(
        "daemon",
        "notify.sound",
        "Sound",
        "Whether a request or a failure plays the system's sound when it reaches the macOS notification centre (the system's Focus and Do Not Disturb still decide whether you hear it). Finished work never makes a sound. A session you muted (its menu: Mute notifications) raises no notification at all, and a session you are looking at raises none either.",
        &[("default", "The system's sound"), ("none", "No sound")],
    ),
    field(
        "daemon",
        "notify.finished",
        "Tell me when work is finished",
        "Also notify, quietly, when an agent finishes a turn or a task: no sound, at most once every 90 seconds for a session, and never for a session you are looking at. Off, only requests and failures notify. The sidebar's Ready for review group is the same either way.",
    ),
    choice(
        "daemon",
        "notify.kinds",
        "What to be told about",
        "The events that raise a notification. Remove the ones you do not want.",
        &[
            ("approval_required", "An agent asks for permission"),
            ("agent_blocked", "An agent is blocked"),
            ("agent_finished", "An agent finished a long turn"),
            ("session_exited_with_error", "A session ended with an error"),
            (
                "chat_turn_finished",
                "A chat answered while you were elsewhere",
            ),
            ("task_done", "A task finished"),
            ("task_failed", "A task failed"),
            ("question_required", "An agent asks you a question"),
        ],
    ),
    field(
        "daemon",
        "notify.min_turn_secs",
        "Shortest turn that counts as finished",
        "An agent's turn must take at least this many seconds before finishing it raises a notification. 0 tells you about every turn.",
    ),
    field(
        "daemon",
        "notify.expire_secs",
        "Forget a notification after",
        "A notification nobody answered lapses after this many seconds. A request for permission stays for as long as the agent waits.",
    ),
    // ---- sessions & restore ----
    choice(
        "daemon",
        "daemon.restore_sessions",
        "Sessions after a restart",
        "What happens to your sessions when the daemon starts again. Terminals start again in their folder and agents resume where they can. “Leave them stopped” brings every session back stopped, with a Restart action.",
        &[
            ("respawn", "Start them again"),
            ("stopped", "Leave them stopped"),
        ],
    ),
    field(
        "daemon",
        "workspaces.home_folder",
        "Home folder",
        "The folder of the Home workspace, where Mawas works for the Home scope. Default: the Hutan folder in your home folder.",
    ),
    choice(
        "daemon",
        "terminal.env_inherit",
        "Environment of new terminals",
        "“Login environment” gives each terminal the environment a fresh login shell sets up, so the tools on your PATH are found. “The daemon's environment” hands over what the daemon itself was started with.",
        &[
            ("login", "Login environment"),
            ("daemon", "The daemon's environment"),
        ],
    ),
    field(
        "daemon",
        "projects.auto",
        "Projects from folders",
        "Place each session in the project of its folder, creating the project the first time. Off, sessions go to Home unless you choose a workspace.",
    ),
    // ---- worktrees (setup and cleanup, WORKTREE-SETUP) ----
    choice(
        "daemon",
        "worktrees.cleanup",
        "Clean up worktrees",
        "Which worktrees Hutan made are offered for removal in Worktree: Clean Up… and by hutan wt prune: “Never”, those whose branch was merged into the default branch, or those untouched for the number of days below. Only worktrees with no live session and no uncommitted changes are offered, each one after its teardown, and nothing is removed without your say-so.",
        &[
            ("never", "Never"),
            ("merged", "After merge"),
            ("idle", "After days idle"),
        ],
    ),
    field(
        "daemon",
        "worktrees.cleanup_idle_days",
        "Days idle before cleanup",
        "With “After days idle”: a worktree is offered once its newest commit and its last git activity are this many days old.",
    ),
    field(
        "daemon",
        "worktrees.wait_for_setup",
        "Agents wait for setup",
        "An agent started in a new worktree waits until the worktree's setup (the commands of .hutan/worktree.toml, shown in a terminal named Setup) has finished. Off, the agent starts at once.",
    ),
    // ---- assistant ----
    choice(
        "daemon",
        "assistant.backend",
        "Backend when nothing is chosen",
        "What answers an assistant that has no choice of its own above. Empty: the first backend that is ready answers.",
        &[
            ("omp", "OMP"),
            ("claude", "Claude Code"),
            ("codex", "Codex"),
            ("provider", "A model provider"),
        ],
    ),
    field(
        "daemon",
        "assistant.provider",
        "Provider when nothing is chosen",
        "The model provider used when the backend is a provider and nothing above names one.",
    ),
    field(
        "daemon",
        "assistant.model",
        "Model when nothing is chosen",
        "The model used when nothing above names one. Empty: Hutan picks a model that suits a read-only helper.",
    ),
    choice(
        "daemon",
        "assistant.mode",
        "Mode",
        "What Mawas may do. Ask before acting: it can change things, and every change asks you first. Advisor: it reads and advises, and changes nothing. A change applies from the next message; Mawas restarts its agent for it.",
        &[("ask", "Ask before acting"), ("advisor", "Advisor")],
    ),
    field(
        "daemon",
        "assistant.max_actions_per_turn",
        "Changes per turn before every one asks",
        "How many changes Mawas may make in one turn (your grants cover them without asking). The next one always asks, whatever is granted: \"Mawas has made 10 changes this turn\".",
    ),
    field(
        "daemon",
        "assistant.context_budget_chars",
        "Size of attachments per message",
        "How many characters of attached context one message may carry, shared between its attachments. Longer attachments are cut.",
    ),
    field(
        "daemon",
        "assistant.idle_stop_secs",
        "Let an idle assistant sleep after",
        "Seconds without use after which an assistant that runs on OMP, Claude Code or Codex stops its agent to give the memory back (several hundred MB). Its conversation stays; the next message wakes it. 0 keeps it running.",
    ),
    field(
        "daemon",
        "assistant.memory",
        "Give Mawas its memory",
        "When on, a Mawas conversation is given the entries you approved (a snapshot at its start, a short update when they change). When off, nothing of the memory is sent to any backend.",
    ),
    field(
        "daemon",
        "assistant.memory_max_entries",
        "Most memory entries",
        "How many active entries Mawas may remember. A new one is refused until you archive another.",
    ),
    field(
        "daemon",
        "assistant.memory_files_in_project",
        "Keep memory in the project",
        "Also write a workspace's memory as Markdown files in its project (.hutan/memory/), where git can see them, so you can read, edit, review and commit them. They are files of your repository: off by default. Mawas's global memory is always written under ~/Hutan/.mawas/memory/. An edit of a file is only ever a proposal you approve in the Memory page.",
    ),
    field(
        "daemon",
        "assistant.context.shell",
        "Projects: terminal output",
        "Lets the assistant of a project read the commands and output of terminals you attach to a message.",
    ),
    field(
        "daemon",
        "assistant.context.agents",
        "Projects: agent transcripts",
        "Lets the assistant of a project read what an agent said and did in a session you attach.",
    ),
    field(
        "daemon",
        "assistant.context.files",
        "Projects: files",
        "Lets the assistant of a project read a file you attach, inside the project's folders only. It can never read outside them.",
    ),
    field(
        "daemon",
        "assistant.context.search",
        "Projects: session search",
        "Lets the assistant of a project use the results of a search of your indexed sessions that you attach.",
    ),
    field(
        "daemon",
        "assistant.global_context.shell",
        "Home: terminal output",
        "Lets the Home assistant read the commands and output of terminals you attach to a message.",
    ),
    field(
        "daemon",
        "assistant.global_context.agents",
        "Home: agent transcripts",
        "Lets the Home assistant read what an agent said and did in a session you attach.",
    ),
    field(
        "daemon",
        "assistant.global_context.files",
        "Home: files",
        "Lets the Home assistant read a file you attach, inside the Home folder only. Off by default, because the Home folder is where private notes tend to be.",
    ),
    field(
        "daemon",
        "assistant.global_context.search",
        "Home: session search",
        "Lets the Home assistant use the results of a search of your indexed sessions that you attach.",
    ),
    // ---- agent control & security ----
    choice(
        "daemon",
        "control.agents",
        "Agents driving Hutan",
        "What an agent running inside a Hutan session may do to Hutan itself: start and stop sessions, type into them, change layouts. “Ask me” asks you each time. “Allow” lets it act without asking. “Read only” lets it look and never change anything. Your own clicks are never affected.",
        &[
            ("ask", "Ask me"),
            ("allow", "Allow"),
            ("read_only", "Read only"),
        ],
    ),
    field(
        "daemon",
        "control.mcp",
        "Give agents Hutan's tools",
        "Agents that Hutan starts get a tool server, so they can drive Hutan under the rule above. Off, they get none and can only use the command line.",
    ),
    field(
        "daemon",
        "control.approval_timeout_secs",
        "Time to answer an agent's request",
        "A request that waits for your answer is refused after this many seconds.",
    ),
    field(
        "daemon",
        "finish.push",
        "Push after Finish",
        "Finish in Changes commits (and merges a worktree), then pushes the branch to its upstream with your own git credentials; it never force-pushes. Off, the commit stays on this Mac until you push it yourself. Agents can never Finish.",
    ),
    // ---- Contain (ADR 0021; Settings → Agent control & security → Contain) ----
    field(
        "daemon",
        "sandbox.network",
        "Network inside Contain",
        "Whether a contained session (a terminal, a command or an OMP chat that you started with Contain on) may use the network when its Start sheet does not say. Off also closes every unix socket. Contain limits where a session can write; it does not stop it reading your files. Read when hutand starts.",
    ),
    field(
        "daemon",
        "sandbox.extra_write",
        "Folders a contained session may also write",
        "Besides its worktree, the repository's git directory (hooks and config stay read-only) and the temp folders: for example ~/.cargo for builds. One folder per line; ~ is your home folder. Read when hutand starts.",
    ),
    field(
        "daemon",
        "sandbox.read_deny",
        "Folders a contained session cannot read",
        "Credential folders by default: ~/.ssh, ~/.aws, ~/.gnupg, ~/.config/gh, ~/.netrc and the Keychain folder. Hutan's own data folder is always closed. Everything else stays readable. Read when hutand starts.",
    ),
    field(
        "daemon",
        "sandbox.bin",
        "Seatbelt tool",
        "The program that applies the boundary; empty is /usr/bin/sandbox-exec. Contain refuses to start a session when it cannot apply a profile, so a wrong path stops contained sessions instead of leaving them uncontained. Read when hutand starts.",
    ),
    // ---- checkpoints (ADR 0019; Settings → Agents & tools → Checkpoints, CK-5) ----
    field(
        "daemon",
        "checkpoints.mode",
        "Checkpoints",
        "Before and after every turn of an agent chat in a git repository, Hutan records the files in a hidden ref, so the turn's edits can be rewound. Files only; edits made by shell commands outside the turn may be missed. Off takes effect from the next turn.",
    ),
    field(
        "daemon",
        "checkpoints.max_file_bytes",
        "Largest file recorded (bytes)",
        "A larger file is left out of a checkpoint and listed.",
    ),
    field(
        "daemon",
        "checkpoints.snapshot_budget_bytes",
        "New content per checkpoint (bytes)",
        "Past this much new content the largest files are left out and listed.",
    ),
    field(
        "daemon",
        "checkpoints.max_files",
        "Changed files per checkpoint",
        "Past this many changed files the largest are left out and listed.",
    ),
    field(
        "daemon",
        "checkpoints.max_untracked_files",
        "Most untracked files",
        "A checkout with more untracked files that are not ignored gets no checkpoints; ignore build output in .gitignore.",
    ),
    field(
        "daemon",
        "checkpoints.exclude",
        "Never recorded",
        "File names left out of every checkpoint, likely secrets. A star matches anything; case is ignored.",
    ),
    field(
        "daemon",
        "checkpoints.keep_turns",
        "Turns kept per chat",
        "Older turns' checkpoints are dropped, the oldest first.",
    ),
    field(
        "daemon",
        "checkpoints.keep_days",
        "Days kept",
        "A chat's checkpoints are dropped after this many days without a new one.",
    ),
    field(
        "daemon",
        "checkpoints.max_repo_bytes",
        "Recorded content per repository (bytes)",
        "Past this, the oldest checkpoints of the repository are dropped. Git removes their content later, under its own settings.",
    ),
    field(
        "daemon",
        "checkpoints.max_repo_refs",
        "Checkpoints per repository",
        "Past this many hidden refs, the oldest checkpoints of the repository are dropped.",
    ),
    // ---- history indexing ----
    field(
        "daemon",
        "history.enabled",
        "Index sessions for search",
        "Lets you search what was said and run in your agents' sessions and in Hutan.",
    ),
    field(
        "daemon",
        "history.hutan",
        "Index Hutan sessions",
        "Include the conversations and terminals of Hutan itself in the index.",
    ),
    field(
        "daemon",
        "history.claude",
        "Index Claude Code sessions",
        "Include the transcripts Claude Code keeps on this Mac.",
    ),
    field(
        "daemon",
        "history.codex",
        "Index Codex sessions",
        "Include the transcripts Codex keeps on this Mac.",
    ),
    field(
        "daemon",
        "history.omp",
        "Index OMP sessions",
        "Include the transcripts OMP keeps on this Mac.",
    ),
    choice(
        "daemon",
        "history.range_days",
        "How far back to index",
        "Only sessions of the last 30 or 90 days, or everything there is.",
        &[
            ("0", "Everything"),
            ("30", "The last 30 days"),
            ("90", "The last 90 days"),
        ],
    ),
    field(
        "daemon",
        "history.roots.claude",
        "Claude Code transcript folder",
        "Where Hutan reads Claude Code's transcripts. Default: Claude Code's own folder.",
    ),
    field(
        "daemon",
        "history.roots.codex",
        "Codex transcript folder",
        "Where Hutan reads Codex's transcripts. Default: Codex's own folder.",
    ),
    field(
        "daemon",
        "history.roots.omp",
        "OMP transcript folder",
        "Where Hutan reads OMP's transcripts. Default: OMP's own folder.",
    ),
    // ---- agents & tools ----
    field(
        "daemon",
        "harness.omp.bin",
        "OMP program",
        "The program Hutan starts to chat with OMP. Default: omp, found on the PATH of your login shell.",
    ),
    field(
        "daemon",
        "harness.omp.args",
        "OMP arguments",
        "Arguments passed to that program. Default: acp.",
    ),
    field(
        "daemon",
        "harness.omp.env",
        "OMP environment",
        "Extra environment variables for OMP. Do not put secrets here: the values stay in daemon.toml as plain text.",
    ),
    field(
        "daemon",
        "harness.omp.cli",
        "OMP command line tool",
        "The tool Hutan runs to list and start OMP logins. Default: the program above, else omp.",
    ),
    field(
        "daemon",
        "harness.omp.account",
        "OMP default account",
        "The account new OMP chats run as when you have not set a default in Accounts: the name of an account you added (see Accounts), or system for the harness's own login. Applies after the daemon restarts.",
    ),
    field(
        "daemon",
        "harness.claude.bin",
        "Claude Code adapter",
        "The program Hutan starts to chat with Claude Code. Default: claude-agent-acp, else claude-code-acp, found on the PATH of your login shell.",
    ),
    field(
        "daemon",
        "harness.claude.args",
        "Claude Code arguments",
        "Arguments passed to that program. Default: none.",
    ),
    field(
        "daemon",
        "harness.claude.env",
        "Claude Code environment",
        "Extra environment variables for Claude Code. Do not put secrets here: the values stay in daemon.toml as plain text.",
    ),
    field(
        "daemon",
        "harness.claude.cli",
        "Claude Code command line tool",
        "The tool Hutan runs to list and start Claude Code logins. Default: claude.",
    ),
    field(
        "daemon",
        "harness.claude.account",
        "Claude Code default account",
        "The account new Claude Code chats run as when you have not set a default in Accounts: the name of an account you added (see Accounts), or system for the harness's own login. Applies after the daemon restarts.",
    ),
    field(
        "daemon",
        "codex.bin",
        "Codex program",
        "The program Hutan starts to chat with Codex. Default: codex, found on the PATH of the daemon.",
    ),
    field(
        "daemon",
        "codex.args",
        "Codex arguments",
        "Arguments passed to that program. Default: app-server.",
    ),
    field(
        "daemon",
        "codex.account",
        "Codex default account",
        "The account new Codex chats run as when you have not set a default in Accounts: the name of an account you added (see Accounts), or system for the harness's own login. Applies after the daemon restarts.",
    ),
    field(
        "daemon",
        "gateway.max_context_chars",
        "Conversation length sent to a model",
        "A direct chat leaves out its oldest messages once a request would be longer than this many characters.",
    ),
    field(
        "daemon",
        "tasks.max_parallel",
        "Tasks that start by themselves",
        "How many tasks set to start automatically may run at the same time.",
    ),
];

fn daemon_entry(key: &str) -> Option<&'static FieldText> {
    DAEMON_FIELDS.iter().find(|f| f.key == key)
}

/// What a permission of an assistant is called and lets it do: the part of a key after
/// `assistant.context.` and so on.
fn permission_text(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "shell" => (
            "terminal output",
            "read the commands and output of terminals you attach",
        ),
        "agents" => (
            "agent transcripts",
            "read what an agent said and did in a session you attach",
        ),
        "files" => (
            "files",
            "read a file you attach, inside this workspace's folders only",
        ),
        "search" => (
            "session search",
            "use the results of a search of your indexed sessions that you attach",
        ),
        _ => return None,
    })
}

/// The part of `assistant.workspaces.<workspace>.<permission>` that names the permission.
fn workspace_permission(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("assistant.workspaces.")?;
    rest.rsplit_once('.').map(|(_, permission)| permission)
}

/// The title of a daemon setting.
pub fn daemon_field_title(key: &str) -> String {
    if let Some(f) = daemon_entry(key) {
        return f.title.to_owned();
    }
    if let Some((what, _)) = workspace_permission(key).and_then(permission_text) {
        return format!("This workspace: {what}");
    }
    if let Some(name) = key.strip_prefix("mcp.") {
        return format!("MCP server “{name}”");
    }
    humanize(key)
}

/// The help under a daemon setting.
pub fn daemon_field_help(key: &str) -> String {
    if let Some(f) = daemon_entry(key) {
        return f.help.to_owned();
    }
    if let Some((_, does)) = workspace_permission(key).and_then(permission_text) {
        return format!(
            "Whether Mawas in this workspace may {does}. Inherit follows the setting for projects."
        );
    }
    if key.starts_with("mcp.") {
        return "A tool server Hutan hands to the agents it starts. It is written in daemon.toml: a program to run, or an address to reach, with its arguments.".to_owned();
    }
    String::new()
}

/// The label of one value of a daemon choice (`ask`, `respawn`, `30`, …).
pub fn daemon_value_label(key: &str, value: &str) -> String {
    daemon_entry(key)
        .and_then(|f| f.values.iter().find(|(v, _)| *v == value))
        .map_or_else(|| humanize(value), |(_, label)| (*label).to_owned())
}

/// What a search of the sheet matches in a daemon setting: its title, help, the labels of its
/// choices and its key.
pub fn daemon_field_search_text(key: &str) -> String {
    let mut text = format!(
        "{} {} {key}",
        daemon_field_title(key),
        daemon_field_help(key)
    );
    if let Some(f) = daemon_entry(key) {
        for (_, label) in f.values {
            text.push(' ');
            text.push_str(label);
        }
    }
    text
}

/// Every key of the daemon wording, for the tests.
pub fn daemon_wording_keys() -> Vec<&'static str> {
    DAEMON_FIELDS.iter().map(|f| f.key).collect()
}

/// `font_size` → `Font Size`: the fallback for a key without an entry.
fn humanize(key: &str) -> String {
    key.rsplit('.')
        .next()
        .unwrap_or(key)
        .split(['_', '-'])
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect::<String>()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The title of the section `namespace` in the sheet (its old page's title).
pub fn namespace_title(namespace: &str) -> String {
    NAMESPACES
        .iter()
        .find(|(ns, _)| *ns == namespace)
        .map_or_else(|| humanize(namespace), |(_, title)| (*title).to_owned())
}

/// The title of the page `page` ([`super::settings_schema::PAGES`]) in the list and the header.
pub fn page_title(page: &str) -> String {
    PAGE_TEXTS
        .iter()
        .find(|(id, _, _)| *id == page)
        .map_or_else(|| humanize(page), |(_, title, _)| (*title).to_owned())
}

/// The line under a page's title in the sheet: what the page is for.
pub fn page_description(page: &str) -> String {
    PAGE_TEXTS
        .iter()
        .find(|(id, _, _)| *id == page)
        .map_or_else(String::new, |(_, _, text)| (*text).to_owned())
}

/// The line under a section's title: what the section is for.
pub fn namespace_description(namespace: &str) -> String {
    NAMESPACE_DESCRIPTIONS
        .iter()
        .find(|(ns, _)| *ns == namespace)
        .map_or_else(String::new, |(_, text)| (*text).to_owned())
}

/// The title of a field; `key` is its dotted path inside the namespace.
pub fn field_title(namespace: &str, key: &str) -> String {
    entry(namespace, key).map_or_else(|| humanize(key), |f| f.title.to_owned())
}

/// The help under a field; `doc` is the schema's own text, used only when there is no entry.
pub fn field_help(namespace: &str, key: &str, doc: &str) -> String {
    entry(namespace, key).map_or_else(|| doc.to_owned(), |f| f.help.to_owned())
}

/// The label of one value of a choice field.
pub fn value_label(namespace: &str, key: &str, value: &str) -> String {
    entry(namespace, key)
        .and_then(|f| f.values.iter().find(|(v, _)| *v == value))
        .map_or_else(
            || {
                // A preset you saved is known by its name, whatever it looks like.
                if (namespace, key) == ("layout", "preset") {
                    value.to_owned()
                } else {
                    humanize(value)
                }
            },
            |(_, label)| (*label).to_owned(),
        )
}

/// Everything a search of the sheet matches in `namespace`'s field: title, help and the labels
/// of its choices (the key itself matches too, for people who know it).
pub fn field_search_text(namespace: &str, key: &str, doc: &str) -> String {
    let mut text = format!(
        "{} {}",
        field_title(namespace, key),
        field_help(namespace, key, doc)
    );
    if let Some(f) = entry(namespace, key) {
        for (_, label) in f.values {
            text.push(' ');
            text.push_str(label);
        }
    }
    text
}

/// What a search of the sheet matches on a page the app draws itself, besides its title: what
/// its controls are called. The pages generated from the schema are searched by their fields.
pub fn page_keywords(page: &str) -> &'static str {
    match page {
        "models" => {
            "Provider providers API key endpoint base URL model default model Keychain \
             Discover local Ollama OpenAI Anthropic add provider"
        }
        "accounts" => {
            "Login logout log in log out account accounts sign in subscription harness \
             OMP Claude Code Codex"
        }
        "data" => {
            "Data folder settings.toml keymap.toml daemon.toml logs folder Reveal in Finder \
             Open database size history index size restart daemon stop daemon"
        }
        "history" => {
            "Index sessions indexing on off sources index size Hutan Claude Code Codex OMP \
             how far back range days transcript folders"
        }
        "experimental" => {
            "Experimental remote hosts SSH host --host hutan host risk disclaimer another Mac"
        }
        "notifications" => {
            "Notify notifications alerts native notification centre Dock badge sound toast \
             requests failures finished ready for review"
        }
        "sessions_restore" => {
            "Restore sessions restart respawn home folder environment login shell projects"
        }
        "updates" => {
            "Updates check for updates new version release GitHub api.github.com Homebrew brew \
             channel stable prerelease pre-release skip version update available"
        }
        "control" => {
            "Agents control approval security MCP tools read only allow ask permission finish \
             push commit git"
        }
        "agents_tools" => {
            "OMP Claude Code Codex program arguments environment MCP servers gateway context \
             tasks parallel checkpoints rewind restore undo retention"
        }
        "assistant" => {
            "Mawas backend model default inherit global workspace permissions files terminal output \
             agent transcripts search budget mode ask before acting advisor actions grants revoke"
        }
        "integrations" => "Hooks install repair uninstall status Claude Code Codex OMP agent hooks",
        "automation" => {
            "Hooks run command script notify phone ntfy sound turn finished failed approval \
             attention session exited task done finish test log timeout focused daemon.toml"
        }
        "launcher" => "Remembered choices Don't ask again revoke exemptions",
        "keymap" => "Bindings shortcuts keys keymap.toml reload open",
        "layout" => "Presets save current layout rename delete",
        "terminal" => "Environment variables env secrets",
        _ => "",
    }
}

/// The words of everything the settings sheet draws besides the fields: pages, buttons,
/// questions and notes. The tests read each of them like the labels above.
pub mod words {
    use std::path::Path;

    macro_rules! texts {
        ($($name:ident = $text:literal;)*) => {
            $(pub const $name: &str = $text;)*
            /// Every text above, for the tests.
            pub const ALL: &[&str] = &[$($name),*];
        };
    }

    texts! {
        SOURCE_DEFAULT = "Default";
        SOURCE_DEFAULT_TIP = "Built in: nothing sets this";
        SOURCE_YOU = "You";
        LAYER_WORKSPACE = "Workspace";
        LAYER_PROJECT = "Project";
        EDIT_IN = "Edit in";
        RESET_TIP = "Reset to default";
        RESET_PAGE = "Reset page…";
        RESET_PAGE_TITLE = "Reset this page?";
        RESET_PAGE_CONFIRM = "Reset page";
        RESET_PAGE_NOTHING = "Nothing on this page is set in the file it edits";
        ADD_CHOICE = "+ add";
        ENV_WARNING = "Do not put secrets here: the values stay in settings.toml as plain text.";
        ENV_ADD = "+ add variable";
        ENV_ADD_HINT = "NAME=value";
        ENV_ADD_FORMAT = "write it as NAME=value";
        ENV_NONE = "No variables set.";
        ENV_EMPTY_VALUE = "(empty)";
        ENV_BAD_NAME = "a name has letters, digits and _, and does not start with a digit";
        ENV_BAD_VALUE = "a value cannot hold a null character";
        HISTORY_INDEX_TITLE = "Index sessions for search";
        HISTORY_INDEX_HELP = "Lets you search what was said and run in your agents' sessions and in Hutan. Off, no transcript folder is opened, read or watched. Turning it off keeps the index it built; clear it from the History panel.";
        HISTORY_UNSUPPORTED = "This daemon cannot index sessions.";
        HISTORY_NO_DAEMON = "The daemon is not running.";
        HISTORY_ASKING = "Asking the daemon…";
        HISTORY_OFF = "Indexing is off.";
        DATA_INTRO = "Everything Hutan keeps on this Mac. Nothing here is changed by looking at it.";
        REVEAL = "Reveal in Finder";
        OPEN = "Open";
        CANNOT_LOCATE = "Cannot tell where this is.";
        PACK_OPEN_FOLDER = "Open pack folder";
        PACK_RELOAD = "Validate / Reload";
        PACK_DUPLICATE = "Duplicate as new pack";
        PACK_RESET = "Reset to embedded default";
        SIZE_UNKNOWN = "not known";
        DAEMON_NOT_RUNNING = "The daemon is not running.";
        SIZES_TITLE = "Size on disk";
        SIZES_HELP = "What the daemon's database and the history index take.";
        DATABASE_NAME = "Database";
        HISTORY_NAME = "History index";
        RESTART_DAEMON = "Restart daemon…";
        STOP_DAEMON = "Stop daemon…";
        DAEMON_TITLE = "Daemon";
        DAEMON_HELP = "The background process that runs your sessions. Restarting or stopping it ends the sessions that are running; it asks first when there are any.";
        INTEGRATIONS_NOTE = "Hooks let a terminal session show what its agent is doing. They are written into each agent's own configuration. Turning off “Keep agent hooks installed on every start” only stops installing: it does not uninstall anything. Use the buttons below to change what is installed.";
        INTEGRATIONS_UNAVAILABLE = "Hooks cannot be changed from this window.";
        HOOKS_CHECKING = "Checking…";
        HOOKS_INSTALLED = "Installed";
        HOOKS_OUTDATED = "Outdated or incomplete";
        HOOKS_NOT_INSTALLED = "Not installed";
        AGENT_NOT_FOUND = "This agent was not found on this Mac; the hooks go into its configuration folder all the same.";
        INSTALL_HOOKS = "Install hooks…";
        REPAIR_HOOKS = "Repair hooks…";
        UNINSTALL_HOOKS = "Uninstall hooks…";
        INSTALL_HOOKS_CONFIRM = "Install";
        REPAIR_HOOKS_CONFIRM = "Repair";
        UNINSTALL_HOOKS_CONFIRM = "Uninstall";
        CONFIRM_ON_TITLE = "Ask again for remembered choices?";
        CONFIRM_ON_FORGET = "Turn on and forget them";
        CONFIRM_ON_KEEP = "Turn on, keep them";
        REVOKE = "Revoke";
        REVOKE_ALL = "Revoke all";
        EXEMPTIONS_TITLE = "Remembered choices";
        EXEMPTIONS_HELP = "Agents and models you chose “Don't ask again” for. A chat on one of them sends its prompt without asking. Revoke one to be asked again.";
        EXEMPTIONS_EMPTY = "None: every agent and model asks first.";
        BINDINGS_TITLE = "Your key bindings";
        BINDINGS_HELP = "The bindings in keymap.toml, on top of the set chosen above: the keys, what they do and where they work. Edit the file to change them, then Reload.";
        BINDINGS_EMPTY = "keymap.toml sets no bindings.";
        BINDING_EVERYWHERE = "Everywhere";
        VIM_KEYS_TITLE = "Vim keys";
        VIM_KEYS_HELP = "With Vim mode on: your Vim mappings from settings.toml first, then Vim's own control keys. Your keymap.toml bindings win over both.";
        VIM_KEYS_OFF = "Vim mode is off: these keys apply once it is on.";
        VIM_MAPPINGS_EMPTY = "settings.toml sets no Vim mappings.";
        VIM_MAPPING_BAD = "not used:";
        VIM_BUILTIN = "Built in";
        OPEN_KEYMAP = "Open keymap.toml";
        RELOAD = "Reload";
        PRESET_NEEDS_NAME = "A preset needs a name";
        PRESET_RENAME_TITLE = "Rename layout preset";
        PRESET_NAME_PROMPT = "New name (letters, digits, - and _):";
        PRESET_RENAME_CONFIRM = "Rename";
        PRESET_IN_USE = "This layout is in use: switch to another one first";
        PRESET_RESTORE = "Restore built-in";
        PRESET_DELETE_CONFIRM = "Delete";
        PRESET_BUILTIN = "Built in";
        PRESET_REPLACED = "Built in, replaced by yours";
        PRESET_SAVED = "Saved by you";
        PRESET_CURRENT = "In use";
        PRESET_RENAME = "Rename…";
        PRESET_DELETE = "Delete…";
        PRESET_RESTORE_SHORT = "Restore built-in…";
        PRESET_SAVE = "Save current layout as…";
        CLEAR_KEY_TITLE = "Clear the API key?";
        CLEAR_KEY_MESSAGE = "The key is removed from the macOS Keychain. Hutan cannot show it again: you will need the key from your provider to use this one again.";
        CLEAR_KEY_CONFIRM = "Clear key";
        STARTER_SETTINGS = "# Hutan settings. Hutan edits this file in place and keeps your comments.\n";
        STARTER_KEYMAP = "# Your key bindings, on top of the set chosen in Settings. For example:\n# [[binding]]\n# keys = \"cmd-shift-d\"\n# action = \"workbench::SplitDown\"\n";
        DAEMON_UNSUPPORTED = "Update the daemon to manage these settings here.";
        DAEMON_ASKING = "Asking the daemon…";
        DAEMON_FILE_BROKEN = "daemon.toml cannot be read";
        OPEN_DAEMON_TOML = "Open daemon.toml";
        CHECK_AGAIN = "Check again";
        RESTART_BANNER = "Some changes apply after the daemon restarts.";
        RESTART_NOW = "Restart now";
        APPLIES_LIVE = "applies at once";
        APPLIES_RESTART = "applies after the daemon restarts";
        PENDING_RESTART = "saved, applies after the daemon restarts";
        SET_IN_FILE = "Set in daemon.toml";
        USE_DEFAULT = "Use default";
        CHOOSE_PATH = "Choose…";
        CHOOSE_PROMPT = "Choose";
        NONE_SET = "none";
        LOOKED_UP = "looked up automatically";
        USUAL_PLACE = "the usual place";
        MCP_NONE = "No MCP servers are set yet.";
        SEC_NOTIFY = "Notifications";
        SEC_SESSIONS = "Sessions";
        SEC_AGENT_CONTROL = "What agents may do to Hutan";
        SEC_HISTORY = "What to index";
        SEC_HISTORY_FOLDERS = "Where the transcripts are";
        SEC_HISTORY_PANEL = "The History panel";
        EXPERIMENTAL_DOCS = "Read about remote hosts";
        SEC_OMP = "OMP";
        SEC_CLAUDE = "Claude Code";
        SEC_CODEX = "Codex";
        SEC_MCP = "MCP servers";
        SEC_LIMITS = "Limits";
        SEC_FINISH = "Finish";
        SEC_CONTAIN = "Contain";
        SEC_CONTAIN_HELP = "Hutan's own Seatbelt boundary for terminals, commands and OMP chats. Writes stay in the worktree; reads stay open except credential folders.";
        SEC_WORKTREES = "Worktrees";
        SEC_CHECKPOINTS = "Checkpoints";
        SEC_CHECKPOINTS_HELP = "What an agent chat's turns record, so their files can be rewound (Rewind… in a turn's footer).";
        SEC_CHECKPOINTS_KEPT = "Checkpoints kept";
        CHECKPOINT_PROJECTS = "A project can differ: [checkpoints.projects.<name or id>] in daemon.toml sets its mode and the four snapshot bounds.";
        CHECKPOINT_CHATS_NOT_ON = "Chats without checkpoints now:";
        SEC_ASSISTANT_PANEL = "Mawas";
        SEC_ASSISTANT_LAYOUT = "Layout";
        SEC_ASSISTANT_ACT = "What Mawas may do";
        SEC_ASSISTANT_ACT_HELP = "Ask before acting: Mawas can change things, and every change asks you first (grants answer for what you allowed). Advisor: Mawas reads and advises, and cannot change anything. A change of mode applies from the next message.";
        SEC_ASSISTANT_GRANTS = "What you allowed Mawas to do without asking";
        SEC_ASSISTANT_GRANTS_HELP = "Granted from an approval card: for one chat, or always in a workspace. Destructive changes always ask, whatever is granted.";
        GRANTS_NONE = "Nothing is granted: Mawas asks before every change.";
        GRANT_REVOKE = "Revoke";
        GRANT_REVOKE_ALL = "Revoke all";
        GRANTS_UNSUPPORTED = "This hutand is older than the app and has no grants: restart it (Daemon → Restart).";
        SEC_HOOKS = "Hooks";
        SEC_HOOKS_HELP = "Commands Hutan runs when something happens: a turn ends, an agent asks, a session exits. You write them in daemon.toml; no agent can add or change them. Hutan gives a command what happened (names, states and counts, never conversation text or secrets) as JSON on its standard input and as HUTAN_EVENT variables.";
        HOOKS_UNSUPPORTED = "This hutand is older than the app and has no hooks: restart it (Daemon → Restart).";
        HOOKS_LOADING = "Asking the daemon…";
        HOOKS_NONE = "No hooks yet. Add a [[hooks]] table to daemon.toml (the Hooks guide has examples, including a phone notification and a sound) and restart the daemon.";
        HOOKS_LOADED_AT_START = "Hooks are read when the daemon starts: after you change them in daemon.toml, restart the daemon for them to take effect.";
        HOOK_NEVER_RAN = "Has not run since the daemon started.";
        HOOK_RUN_TEST = "Run test";
        HOOK_TESTING = "Running…";
        HOOKS_REFRESH = "Refresh";
        SEC_HOOKS_LOG = "Log";
        SEC_HOOKS_LOG_HELP = "The latest runs, newest first, with what each printed (secrets masked). The log lasts until the daemon stops.";
        HOOKS_LOG_EMPTY = "Nothing has run yet.";
        SEC_ASSISTANT_BACKEND = "Backend and model";
        SEC_ASSISTANT_BACKEND_HELP = "What answers Mawas in each place. Inherit follows the place above it: a workspace follows its parent workspace, then Global, then daemon.toml, then Automatic, the first backend that is ready.";
        SEC_ASSISTANT_DEFAULT = "When nothing is chosen";
        SEC_ASSISTANT_PROJECTS = "What Mawas may read in projects";
        SEC_ASSISTANT_HOME = "What Mawas may read at Home";
        SEC_ASSISTANT_THIS = "What Mawas may read in this workspace";
        SEC_ASSISTANT_THIS_HELP = "Overrides for this workspace only. Inherit follows the setting for projects.";
        SEC_ASSISTANT_LIMITS = "Attachments";
        SEC_ASSISTANT_MEMORY = "Memory";
        SEC_ASSISTANT_MEMORY_HELP = "What Mawas remembers because you approved it: see and edit it on the Memory page (Mawas: Memory). A model's proposals are never saved until you approve them. Archived entries are no longer given to new conversations; text already sent to a provider or kept in a transcript cannot be taken back.";
        SCOPE_GLOBAL = "Global (Home)";
        INHERIT = "Inherit";
        ALLOW = "Allow";
        DENY = "Deny";
        AUTOMATIC = "Automatic";
        BACKEND_OMP = "OMP";
        BACKEND_CLAUDE = "Claude Code";
        BACKEND_CODEX = "Codex";
        BACKEND_PROVIDER = "Provider";
        MODEL_TITLE = "Model";
        MODEL_LOADING = "Loading the models…";
        MODEL_NONE = "No models were listed.";
        PROVIDERS_NONE = "No model provider is set up: add one in Models.";
        NOT_OPENED = "Not opened yet: Mawas starts on its backend the first time you use it.";
        NO_PROJECT_NOTE = "Open a project to set its own backend and permissions here.";
        NO_MATCH_HINT = "Clear the search to see every page, or try another word.";
        NO_MATCH_FOOTER = "No matches";
        CLEAR_SEARCH = "Clear search";
        FILTER_MODIFIED_TIP = "Only the settings you changed: a settings file sets them (the ↺ takes one back)";
        FILTER_RESTART_TIP = "Only the settings that apply after the daemon restarts";
        MCP_HELP = "Tool servers Hutan hands to the agents it starts: a program to run, or an address to reach. They are written in daemon.toml and reach agent sessions once the daemon has restarted.";
        MCP_ADD = "Add server…";
        MCP_ADD_TITLE = "Add an MCP server";
        MCP_EDIT_TITLE = "Edit the MCP server";
        MCP_EDIT = "Edit";
        MCP_REMOVE = "Remove…";
        MCP_REMOVE_CONFIRM = "Remove";
        MCP_REMOVE_MESSAGE = "The server is taken out of daemon.toml. Agent sessions started after the daemon restarts will not get it.";
        MCP_REMOVED = "Removed from the file; it stays until the daemon restarts";
        MCP_NAME = "Name";
        MCP_NAME_HINT = "docs";
        MCP_KIND = "Runs as";
        MCP_KIND_PROGRAM = "A program on this Mac";
        MCP_KIND_REMOTE = "A remote server";
        MCP_COMMAND = "Program";
        MCP_COMMAND_HINT = "npx";
        MCP_ARGS = "Arguments";
        MCP_ARGS_HINT = "-y @example/docs-server";
        MCP_URL = "Address";
        MCP_URL_HINT = "https://example.com/mcp";
        MCP_TRANSPORT = "Transport";
        MCP_HTTP = "HTTP";
        MCP_SSE = "SSE";
        MCP_CREATE = "Add server";
        MCP_SAVE = "Save";
        MCP_CANCEL = "Cancel";
        MCP_NEEDS_NAME = "Give the server a name first (lowercase letters, digits, - and _).";
        MCP_NEEDS_COMMAND = "Type the program to run.";
        MCP_NEEDS_URL = "Type the address of the server.";
        MCP_QUOTE_OPEN = "A quote in the arguments is not closed.";
        // ---- R4-A ----
        ENV_WARNING_DAEMON = "Do not put secrets here: the values stay in daemon.toml as plain text.";
        IDLE_STOP_HINT = "0 (never) or at least 60";
        PROBLEMS_COUNT = "Problems";
        APPLIES_RESTART_SHORT = "after restart";
        PENDING_RESTART_SHORT = "restart to apply";
        PACK_MORE_TIP = "Pack actions";
        PACK_HINT = "↑ ↓ preview · Return applies · Esc cancels";
        PROVIDER_DETAILS = "Details";
        RESTART_NEUTRAL = "What happens to them in the new daemon depends on the setting “Sessions after a restart”.";
        RESTART_RESPAWN = "The new daemon restores them: shells start again in their folders, chats resume their conversations.";
        RESTART_STOPPED = "The new daemon lists them stopped (your setting “Leave them stopped”); start each again from its Restart action.";
    }

    /// "Last run: ok in 212 ms, 3m ago".
    pub fn hook_last_run(run: &str) -> String {
        format!("Last run: {run}")
    }

    /// "4 runs, 1 failed, 2 dropped (queue full), 3 skipped (window focused)"; nothing before
    /// the first run.
    pub fn hook_counts(runs: u64, failures: u64, dropped: u64, skipped: u64) -> Option<String> {
        if runs == 0 && dropped == 0 && skipped == 0 {
            return None;
        }
        let mut parts = vec![format!("{runs} {}", if runs == 1 { "run" } else { "runs" })];
        if failures > 0 {
            parts.push(format!("{failures} failed"));
        }
        if dropped > 0 {
            parts.push(format!("{dropped} dropped (queue full)"));
        }
        if skipped > 0 {
            parts.push(format!("{skipped} skipped (window focused)"));
        }
        Some(parts.join(", "))
    }

    /// "1 running now. At most 4 run at once and 8 wait for each hook; more are dropped."
    pub fn hooks_limits(running: u32, max: u32, queue: u32) -> String {
        format!(
            "{running} running now. At most {max} run at once and {queue} wait for each hook; further events are dropped and counted. Hooks are read when the daemon starts."
        )
    }

    /// "Remove MCP server “docs”?".
    pub fn mcp_remove_title(name: &str) -> String {
        format!("Remove MCP server “{name}”?")
    }

    /// "Workspace api", or just "Workspace" when the container's name is not known.
    pub fn layer_named(layer: &str, name: Option<&String>) -> String {
        match name {
            Some(name) => format!("{layer} {name}"),
            None => layer.to_owned(),
        }
    }

    /// "changes go to Project api": the layer a change to a field goes to, when that is not
    /// where the value shown comes from.
    pub fn editing(layer: &str) -> String {
        format!("changes go to {layer}")
    }

    /// "Until then, edit /path by hand.": where the file is when this daemon cannot change it.
    pub fn daemon_file_line(path: &str) -> String {
        format!("Until then, edit {path} by hand.")
    }

    /// "Default: On": what the setting is when daemon.toml does not set it.
    pub fn default_is(value: &str) -> String {
        format!("Default: {value}")
    }

    /// "Set in daemon.toml (default: On)".
    pub fn set_with_default(default: &str) -> String {
        format!("{SET_IN_FILE} (default: {default})")
    }

    /// "Runs on OMP · model": what the assistant of a place runs on.
    pub fn runs_on(backend: &str, model: Option<&str>) -> String {
        match model {
            Some(model) => format!("Runs on {backend} · {model}"),
            None => format!("Runs on {backend}"),
        }
    }

    /// What "Inherit" follows, and where it comes from.
    pub fn inherits(from: &str, backend: Option<&str>) -> String {
        match backend {
            Some(backend) => format!("Inherits {from}: {backend}"),
            None => format!("Inherits {from}"),
        }
    }

    /// "x (not in the list)": a stored model the catalog does not list.
    pub fn model_unlisted(model: &str) -> String {
        format!("{model} (not in the list)")
    }

    /// "Automatic · runs gpt-5": the model Automatic picked.
    pub fn automatic_runs(model: &str) -> String {
        format!("Automatic · runs {model}")
    }

    /// What an assistant permission follows when it is not overridden.
    pub fn inheriting_permission(allowed: bool) -> String {
        format!(
            "Inherits the setting for projects: {}",
            if allowed { "allowed" } else { "not allowed" }
        )
    }

    /// A value the app does not offer.
    pub fn not_available(value: &str) -> String {
        format!("{value} (not available)")
    }

    pub fn range_hint(low: f64, high: f64) -> String {
        format!("{} to {}", number(low), number(high))
    }

    /// "at least 1": the lower end of a number that has no upper end.
    pub fn at_least(low: i64) -> String {
        format!("at least {low}")
    }

    /// "Automatic · OMP · model": the backend was picked by Hutan, not chosen.
    pub fn automatic_runs_on(backend: &str, model: Option<&str>) -> String {
        match model {
            Some(model) => format!("Automatic · {backend} · {model}"),
            None => format!("Automatic · {backend}"),
        }
    }

    /// "No settings match “word”": the page that stands in while the search finds nothing.
    pub fn no_match(query: &str) -> String {
        format!("No settings match “{query}”")
    }

    fn number(n: f64) -> String {
        if n.fract() == 0.0 {
            format!("{n:.0}")
        } else {
            format!("{n}")
        }
    }

    /// What "Reset page" asks, listing what goes.
    pub fn reset_page_message(listing: &[String]) -> String {
        format!(
            "These settings are removed from the file they are set in, so each goes back to what the layer below says, or to its default: {}.",
            listing.join(", ")
        )
    }

    /// A variable that is not set in the layer being edited.
    pub fn map_row_elsewhere(name: &str, layer: &str) -> String {
        format!("{name} is not set in {layer}: it comes from another layer")
    }

    /// A number that was typed outside its range and kept inside it.
    pub fn kept_within(field: &str, low: f64, high: f64) -> String {
        format!(
            "{field} is kept between {} and {}",
            number(low),
            number(high)
        )
    }

    /// The title of a field as a name inside a sentence.
    pub fn field_name(namespace: &str, key: &str) -> String {
        super::field_title(namespace, key)
    }

    pub fn history_toggle_failed(enabled: bool, error: &str) -> String {
        format!(
            "Cannot {} indexing: {error}",
            if enabled { "turn on" } else { "turn off" }
        )
    }

    /// "Indexed: omp 12 rows, claude 3 rows · 2.4 MB".
    pub fn history_indexed(sources: &[(String, u64)], size: &str) -> String {
        let per_source: Vec<String> = sources
            .iter()
            .map(|(name, rows)| format!("{name} {rows} rows"))
            .collect();
        if per_source.is_empty() {
            format!("Indexing is on; nothing indexed yet · {size}")
        } else {
            format!("Indexed: {} · {size}", per_source.join(", "))
        }
    }

    pub fn not_there_yet(path: &Path) -> String {
        format!("{} does not exist yet", path.display())
    }

    pub fn cannot_create(path: &Path, error: &str) -> String {
        format!("Cannot create {}: {error}", path.display())
    }

    /// Title and help of the `i`th entry of the Data & logs page.
    pub fn data_target(i: usize) -> (&'static str, &'static str) {
        match i {
            0 => (
                "Data folder",
                "Where Hutan keeps its database, settings, history index and the daemon's files.",
            ),
            1 => (
                "settings.toml",
                "Your settings. Hutan edits it in place and keeps your comments.",
            ),
            2 => ("keymap.toml", "Your own key bindings."),
            3 => (
                "daemon.toml",
                "The daemon's configuration. The daemon pages of Settings edit it and keep your comments; a few changes apply after the daemon restarts.",
            ),
            _ => (
                "Logs folder",
                "desktop.log and hutand.log: what to look at when something goes wrong.",
            ),
        }
    }

    /// "hutand 0.31.0 · pid 812 · up 3h 12m · 4 sessions".
    pub fn daemon_summary(version: &str, pid: u32, uptime_ms: u64, sessions: u32) -> String {
        let minutes = uptime_ms / 60_000;
        let up = if minutes >= 60 {
            format!("{}h {}m", minutes / 60, minutes % 60)
        } else {
            format!("{minutes}m")
        };
        format!(
            "Daemon {version} · pid {pid} · up {up} · {sessions} session{}",
            if sessions == 1 { "" } else { "s" }
        )
    }

    pub fn size_line(name: &str, size: &str) -> String {
        format!("{name}: {size}")
    }

    pub fn hook_place(place: &str) -> String {
        format!("Changes: {place}")
    }

    pub fn hooks_help(agent: &str) -> String {
        format!("Hooks that tell Hutan what {agent} is doing in a terminal session.")
    }

    pub fn install_title(agent: &str, repair: bool) -> String {
        if repair {
            format!("Repair the hooks of {agent}?")
        } else {
            format!("Install the hooks of {agent}?")
        }
    }

    pub fn install_message(writes: &str, place: &str) -> String {
        format!(
            "Writes {writes}: {place}. Only Hutan's own entries are written, and a file that is changed is backed up first."
        )
    }

    pub fn uninstall_title(agent: &str) -> String {
        format!("Uninstall the hooks of {agent}?")
    }

    pub fn uninstall_message(place: &str) -> String {
        format!(
            "Removes Hutan's own entries and files from: {place}. Nothing else in them is touched."
        )
    }

    pub fn confirm_on_message(remembered: usize) -> String {
        format!(
            "{remembered} agent and model choice{} you chose “Don't ask again” for would keep skipping the question. Forget {}?",
            if remembered == 1 { "" } else { "s" },
            if remembered == 1 { "it" } else { "them" }
        )
    }

    /// "OMP · gpt-5".
    pub fn exemption(agent: &str, model: &str) -> String {
        format!("{agent} · {model}")
    }

    pub fn preset_exists(name: &str) -> String {
        format!("A preset named {name} exists already")
    }

    pub fn preset_missing(name: &str) -> String {
        format!("There is no saved preset {name}")
    }

    pub fn preset_renamed(from: &str, to: &str) -> String {
        format!("Renamed layout preset {from} to {to}")
    }

    pub fn preset_deleted(name: &str) -> String {
        format!("Deleted layout preset {name}")
    }

    pub fn preset_delete_title(name: &str, builtin: bool) -> String {
        if builtin {
            format!("Restore the built-in {name} layout?")
        } else {
            format!("Delete the layout preset {name}?")
        }
    }

    /// What restarting the daemon does to the live sessions, following the restore policy
    /// (`daemon.restore_sessions`; `None` while the daemon's settings are not known).
    pub fn restart_message(sessions: &str, restore_policy: Option<&str>) -> String {
        match restore_policy {
            Some("respawn") => format!(
                "{sessions} will be stopped and running commands interrupted. {RESTART_RESPAWN}"
            ),
            Some("stopped") => format!("{sessions} will be stopped. {RESTART_STOPPED}"),
            _ => format!(
                "{sessions} will be stopped and running commands interrupted. {RESTART_NEUTRAL}"
            ),
        }
    }

    pub fn preset_delete_message(builtin: bool) -> &'static str {
        if builtin {
            "Your saved version is removed and the built-in layout of that name is used again."
        } else {
            "The preset is removed from settings.toml. The layout on screen does not change."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::settings_schema::{
        FieldKind, PAGES, catalog, fields_of, namespace_schema,
    };
    use hutan_settings::{SettingsPaths, SettingsStore};

    /// A word with an underscore between lowercase parts (`on_last_window_close`).
    fn has_snake_case(text: &str) -> bool {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|word| {
                word.contains('_')
                    && word
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && word.trim_matches('_').contains('_')
            })
    }

    fn assert_user_facing(what: &str, text: &str) {
        assert!(!text.trim().is_empty(), "{what}: empty");
        assert!(!text.contains("::"), "{what}: `{text}` has `::`");
        assert!(!text.contains('`'), "{what}: `{text}` has a code span");
        assert!(!has_snake_case(text), "{what}: `{text}` has a key name");
        assert!(
            !text.contains("docs/"),
            "{what}: `{text}` names a repo file"
        );
    }

    #[test]
    fn every_namespace_field_and_choice_has_user_facing_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
        crate::settings::register_all(&store);
        let schema = store.schema();
        let mut fields = 0;
        for page in PAGES {
            let id = page.id;
            assert!(
                PAGE_TEXTS.iter().any(|(p, _, _)| *p == id),
                "page {id} has no title and description"
            );
            assert_user_facing(&format!("page {id}"), &page_title(id));
            let description = page_description(id);
            assert_user_facing(&format!("page {id} description"), &description);
            assert!(
                description.chars().count() <= 80,
                "page {id}: the description is one line under the title: `{description}`"
            );
        }
        for entry in catalog() {
            let ns = entry.namespace;
            assert!(
                NAMESPACES.iter().any(|(n, _)| *n == ns),
                "namespace {ns} has no title"
            );
            assert_user_facing(&format!("namespace {ns}"), &namespace_title(ns));
            assert!(
                NAMESPACE_DESCRIPTIONS.iter().any(|(n, _)| *n == ns),
                "namespace {ns} has no description"
            );
            let description = namespace_description(ns);
            assert_user_facing(&format!("namespace {ns} description"), &description);
            assert!(
                description.chars().count() <= 80,
                "namespace {ns}: the description is one line under the title: `{description}`"
            );
            let Some(namespace_schema) = namespace_schema(&schema, ns) else {
                continue;
            };
            for field in fields_of(&namespace_schema) {
                let key = field.dotted();
                let what = format!("{ns}.{key}");
                assert!(self::entry(ns, &key).is_some(), "{what} has no wording");
                assert_user_facing(&format!("{what} title"), &field_title(ns, &key));
                assert_user_facing(&format!("{what} help"), &field_help(ns, &key, &field.doc));
                if let FieldKind::Enum(values) = &field.kind {
                    for value in values {
                        assert!(
                            self::entry(ns, &key)
                                .is_some_and(|f| f.values.iter().any(|(v, _)| v == value)),
                            "{what}: choice `{value}` has no label"
                        );
                        assert_user_facing(
                            &format!("{what} choice {value}"),
                            &value_label(ns, &key, value),
                        );
                    }
                }
                fields += 1;
            }
        }
        assert!(fields > 50, "only {fields} fields were checked");
        // Nothing in the table is left over from a field that is gone.
        for f in FIELDS {
            let exists = catalog()
                .iter()
                .filter(|e| e.namespace == f.namespace)
                .filter_map(|e| namespace_schema(&schema, e.namespace))
                .flat_map(|s| fields_of(&s))
                .any(|field| field.dotted() == f.key);
            assert!(exists, "{}.{} is not a field", f.namespace, f.key);
        }
    }

    #[test]
    fn every_new_text_reads_like_the_rest() {
        // The starter files are file contents, not wording.
        for text in words::ALL.iter().filter(|t| !t.starts_with("# ")) {
            assert_user_facing("a text of the sheet", text);
        }
        for page in [
            "data",
            "models",
            "accounts",
            "history",
            "integrations",
            "automation",
        ] {
            assert!(!page_keywords(page).trim().is_empty(), "{page}");
            assert_user_facing(&format!("keywords of {page}"), page_keywords(page));
        }
        let path = std::path::Path::new("/tmp/hutan");
        let sentences = [
            words::layer_named(words::LAYER_PROJECT, Some(&"api".to_owned())),
            words::editing("Project api"),
            words::not_available("x"),
            words::range_hint(4.0, 72.0),
            words::reset_page_message(&["Font size (You)".to_owned()]),
            words::map_row_elsewhere("TOKEN", "You"),
            words::kept_within("Font size", 4.0, 72.0),
            words::history_toggle_failed(true, "no"),
            words::history_toggle_failed(false, "no"),
            words::history_indexed(&[("omp".to_owned(), 3)], "2.0 KB"),
            words::history_indexed(&[], "0 B"),
            words::not_there_yet(path),
            words::cannot_create(path, "denied"),
            words::daemon_summary("0.1.0", 7, 7_500_000, 1),
            words::size_line("Database", "1.0 KB"),
            words::hook_place("~/.codex/hooks.json"),
            words::hooks_help("Codex"),
            words::install_title("Codex", false),
            words::install_title("Codex", true),
            words::install_message("a file", "~/.codex/x"),
            words::uninstall_title("Codex"),
            words::uninstall_message("~/.codex/x"),
            words::confirm_on_message(1),
            words::confirm_on_message(3),
            words::exemption("OMP", "default"),
            words::preset_exists("x"),
            words::preset_missing("x"),
            words::preset_renamed("a", "b"),
            words::preset_deleted("a"),
            words::preset_delete_title("a", false),
            words::preset_delete_title("focus", true),
            words::preset_delete_message(false).to_owned(),
            words::preset_delete_message(true).to_owned(),
            words::field_name("terminal", "font_size"),
            words::daemon_file_line("/tmp/hutan/daemon.toml"),
            words::default_is("On"),
            words::set_with_default("On"),
            words::runs_on("OMP", Some("model")),
            words::runs_on("OMP", None),
            words::automatic_runs_on("OMP", Some("model")),
            words::inherits("Global", Some("OMP")),
            words::inherits("Global", None),
            words::model_unlisted("x"),
            words::automatic_runs("x"),
            words::inheriting_permission(true),
            words::inheriting_permission(false),
            words::at_least(1),
            words::no_match("zzz"),
            words::mcp_remove_title("docs"),
            words::hook_last_run("ok in 212 ms, 3m ago"),
            words::hook_counts(4, 1, 2, 3).unwrap(),
            words::hook_counts(1, 0, 0, 0).unwrap(),
            words::hooks_limits(1, 4, 8),
        ];
        assert!(words::hook_counts(0, 0, 0, 0).is_none());
        for sentence in &sentences {
            assert_user_facing("a sentence of the sheet", sentence);
        }
        for i in 0..5 {
            let (title, help) = words::data_target(i);
            assert_user_facing("a data title", title);
            assert_user_facing("a data help", help);
        }
    }

    #[test]
    fn every_choice_the_app_offers_has_a_label() {
        use crate::workbench::settings_schema::refine_kind;
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
        crate::settings::register_all(&store);
        let schema = store.schema();
        for entry in catalog() {
            let Some(namespace_schema) = namespace_schema(&schema, entry.namespace) else {
                continue;
            };
            for field in fields_of(&namespace_schema) {
                let key = field.dotted();
                let values = match refine_kind(entry.namespace, &key, &field.kind) {
                    FieldKind::Enum(values) | FieldKind::ChoiceList(values) => values,
                    _ => continue,
                };
                for value in values {
                    assert!(
                        self::entry(entry.namespace, &key)
                            .is_some_and(|f| f.values.iter().any(|(v, _)| *v == value)),
                        "{}.{key}: choice `{value}` has no label",
                        entry.namespace
                    );
                }
            }
        }
        // A saved preset is shown as its name, not as a title made from it.
        assert_eq!(value_label("layout", "preset", "my-layout"), "my-layout");
        assert_eq!(value_label("layout", "preset", "focus"), "Focus");
    }

    #[test]
    fn the_fallbacks_read_like_titles() {
        assert_eq!(humanize("font_size"), "Font Size");
        assert_eq!(humanize("cursor.blink"), "Blink");
        assert_eq!(field_title("terminal", "font_size"), "Font size");
        assert_eq!(
            value_label("app", "on_last_window_close", "menu_bar"),
            "Keep in the menu bar"
        );
        assert_eq!(value_label("nowhere", "x", "some_value"), "Some Value");
    }

    #[test]
    fn the_restart_question_follows_the_restore_policy_in_words() {
        let respawn = words::restart_message("2 live sessions", Some("respawn"));
        assert_eq!(
            respawn,
            "2 live sessions will be stopped and running commands interrupted. The new daemon restores them: shells start again in their folders, chats resume their conversations."
        );
        let stopped = words::restart_message("2 live sessions", Some("stopped"));
        assert_eq!(
            stopped,
            "2 live sessions will be stopped. The new daemon lists them stopped (your setting “Leave them stopped”); start each again from its Restart action."
        );
        // Not known yet: no promise either way.
        let unknown = words::restart_message("2 live sessions", None);
        assert!(!unknown.contains("restores them") && !unknown.contains("lists them stopped"));
        assert!(unknown.starts_with("2 live sessions will be stopped"));
    }

    #[test]
    fn the_variables_warning_names_the_file_that_keeps_them() {
        assert!(words::ENV_WARNING.contains("settings.toml"));
        assert!(!words::ENV_WARNING.contains("daemon.toml"));
        assert!(words::ENV_WARNING_DAEMON.contains("daemon.toml"));
        assert!(!words::ENV_WARNING_DAEMON.contains("settings.toml"));
    }

    #[test]
    fn the_checker_rejects_what_it_is_for() {
        assert!(has_snake_case("see on_last_window_close"));
        assert!(!has_snake_case("Font size, in points."));
    }

    #[test]
    fn every_daemon_key_has_user_facing_wording() {
        let keys = daemon_wording_keys();
        for key in &keys {
            assert_user_facing(&format!("{key} title"), &daemon_field_title(key));
            assert_user_facing(&format!("{key} help"), &daemon_field_help(key));
            let entry = daemon_entry(key).unwrap();
            for (value, label) in entry.values {
                assert!(!value.is_empty());
                assert_user_facing(&format!("{key} choice {value}"), label);
            }
            assert!(daemon_field_search_text(key).contains(key));
        }
        // The keys that depend on the file's contents.
        for key in [
            "assistant.workspaces.proj.shell",
            "assistant.workspaces.proj.agents",
            "assistant.workspaces.proj.files",
            "assistant.workspaces.proj.search",
            "mcp.docs",
        ] {
            assert_user_facing(&format!("{key} title"), &daemon_field_title(key));
            assert_user_facing(&format!("{key} help"), &daemon_field_help(key));
        }
        assert_eq!(
            daemon_value_label("control.agents", "read_only"),
            "Read only"
        );
        assert_eq!(
            daemon_value_label("history.range_days", "30"),
            "The last 30 days"
        );
    }
}
