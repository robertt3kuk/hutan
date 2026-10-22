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
    ("panels.sessions", "Projects Sidebar"),
    ("sidebar", "Sidebar organization"),
    ("panels.inspector", "Requests"),
    ("panels.files", "Files"),
    ("panels.problems", "Problems"),
    ("panels.search", "Search"),
    ("workspaces", "Workspaces"),
    ("history", "History"),
    ("chat", "Chat"),
    ("editor", "Editing"),
    ("agents", "Agents"),
    ("resources", "Resources"),
    ("tasks", "Tasks"),
    ("assistant", "Mawas"),
    ("integrations", "Agent Integrations"),
    ("launcher", "Start Sheet"),
    ("accounts", "Accounts"),
    ("models", "Models"),
    ("notifications", "Notifications"),
    ("sessions_restore", "Sessions & restore"),
    ("control", "Agent control & security"),
    ("agents_tools", "Agents & tools"),
];

const FIELDS: &[FieldText] = &[
    // ---- appearance ----
    choice(
        "appearance",
        "density",
        "Density",
        "How much space rows, buttons and panels take. Compact fits more on screen; Comfortable uses larger rows and more padding.",
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
        "Whether menus, sheets, toasts and groups fade and slide. System follows macOS (System Settings, Accessibility, Display) and changes with it at once. Reduced, nothing slides or changes height and fades stay under 150 ms.",
        &[("system", "System"), ("on", "On"), ("off", "Off")],
    ),
    field(
        "appearance",
        "light_theme",
        "Light theme",
        "The color theme of light windows. Moving through the list previews each theme without changing anything; Return applies it. A theme that is missing or invalid is replaced by Hutan Light until it works.",
    ),
    field(
        "appearance",
        "dark_theme",
        "Dark theme",
        "The color theme of dark windows. Moving through the list previews each theme without changing anything; Return applies it. A theme that is missing or invalid is replaced by Hutan Dark until it works.",
    ),
    field(
        "appearance",
        "icon_theme",
        "Icon theme",
        "The icons, chosen independently of the colors. Icons a theme leaves out fall back to Phosphor one by one; the preview shows which ones. Return applies it.",
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
        "Quitting Hutan leaves its background daemon and your sessions running, without asking. You can stop it from the Daemon menu or with “hutan daemon stop”. Off, the daemon stops when Hutan quits.",
    ),
    field(
        "app",
        "start_at_login",
        "Start the daemon when you log in",
        "Registers Hutan's launch agent in macOS Login Items so the daemon starts at login and restarts after a crash. While it is registered, quitting Hutan never stops the daemon. Only available when Hutan runs from Hutan.app.",
    ),
    field(
        "app",
        "setup_done",
        "First run finished",
        "Whether the first run is over: you opened a folder or started a terminal from the first screen, or finished or skipped the setup. Turn it off to see the first screen again at the next start, or use “Run Setup Again…” below for the setup.",
    ),
    field(
        "app",
        "ask_notifications",
        "Offer notifications",
        "Shows a one-line offer once, the first time something wants you (an approval, a finished run) while Hutan is not the frontmost app. The system's permission prompt only appears when you press “Notify me”. Off after you answered or dismissed it.",
    ),
    field(
        "app",
        "ask_hooks",
        "Offer hooks for an agent in a terminal",
        "Shows an offer once, in the strip under a terminal where Codex, Claude Code or OMP runs without having reported: install that agent's hooks, which says what is written and where first. Off after you answered or dismissed it. Nothing is written unless you accept.",
    ),
    choice(
        "app",
        "on_last_window_close",
        "When the last window closes",
        "Quit Hutan, which asks first while sessions are running, or keep Hutan in the menu bar without a Dock icon. Keeping it in the menu bar needs the menu bar item below.",
        &[("quit", "Quit Hutan"), ("menu_bar", "Keep in the menu bar")],
    ),
    field(
        "app",
        "menu_bar",
        "Show the menu bar item",
        "A menu bar icon with the daemon's state, how many sessions have a request waiting or an unseen failure, and a menu to jump to them. While no Hutan window is connected, the daemon shows its own icon whatever this says.",
    ),
    field(
        "notifications",
        "dock_badge",
        "Show a badge on the Dock icon",
        "Shows how many sessions have a request waiting (an approval, a question, a plan) on the Dock icon, and nothing at zero. Only requests count: a failure or finished work never does.",
    ),
    // ---- keyboard ----
    choice(
        "keymap",
        "base",
        "Key bindings",
        "The set of shortcuts Hutan starts from. Zed keys follow Zed on macOS (⌘⇧P command palette, ⌘P jump, ⌘B left sidebar, ⌘L ⌘R right sidebar, ⌘J bottom dock); Hutan keys are the original ones (⌘K command palette, ⌘J jump). Your own bindings in keymap.toml are applied on top, and a change takes effect at once.",
        &[("zed", "Zed keys"), ("hutan", "Hutan keys")],
    ),
    // ---- terminal ----
    field(
        "terminal",
        "font_family",
        "Font",
        "The font of terminal text. Use a monospaced font.",
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
        "The terminal's colors. Automatic uses the terminal colors of the color theme in effect (so it follows the light or dark mode); any installed color theme keeps its terminal colors whatever the window does.",
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
        "Makes the Option key send Meta (Alt) for shell and editor shortcuts instead of typing special characters. macOS cannot tell the two Option keys apart here, so every choice except None applies to both.",
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
        "Closing a terminal tab",
        "What the × of a terminal tab does. Detach closes the tab only: the session keeps running and stays in the sidebar. Close session also ends the session and removes it from the sidebar (it asks first when something runs in it; History can reopen it). The tab's context menu and the sidebar's × always offer Close session….",
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
        "dim_unfocused_splits",
        "Dim unfocused splits",
        "In a terminal tab with splits, the splits that do not have the keyboard lose a few percent of their contrast, so the one you type in stands out by more than its top edge. Off, only the edge marks it.",
    ),
    field(
        "terminal",
        "blocks",
        "Command blocks",
        "Groups each command and its output into a block with a marker bar, a status chip, and actions on hover such as copy and fold. Off, the terminal looks like a plain terminal.",
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
        "Variables added to the environment of new terminals, on top of your login environment. Terminals that are already open keep theirs. A workspace or project can set its own value for a variable.",
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
        "offer_new_default",
        "Offer the new default layout",
        "Shows a one-line offer once when your saved layout predates the quieter default (Changes, Agents and History on the right rail, Requests and Resources under ⋯): “Use new default” or “Keep mine”. Your layout stays as it is unless you choose it. Off after you answered.",
    ),
    field(
        "layout",
        "presets",
        "Your layout presets",
        "Layouts you saved, by name. Save the layout you have on screen, rename a preset or delete one. A saved preset named like a built-in one replaces it until you delete it.",
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
        "Shows the groups on top of the sidebar: Requests (an approval, a question or a plan waits for you), Failed and Ready for review (finished work you have not seen yet).",
    ),
    choice(
        "panels.sessions",
        "row",
        "Row contents",
        "What each row shows, in order. The row is one line of state, kind, title and age. A second line appears only when it has news: folder (where the session is now, once it has left its checkout) and placement (a worktree branch, or a shared main checkout). Pick the parts from the menu.",
        &[
            ("state", "State"),
            ("kind", "Kind"),
            ("title", "Title"),
            ("folder", "Folder"),
            ("placement", "Placement"),
            ("age", "Age"),
        ],
    ),
    choice(
        "sidebar",
        "organize",
        "Organize the sidebar",
        "Workspaces first lists your workspaces with their projects inside, then the projects that are in no workspace, then Home. Projects only shows one flat list of projects, pinned ones first, then Home.",
        &[
            ("workspaces", "Workspaces, then projects"),
            ("projects", "Projects only"),
        ],
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
        "auto_reveal",
        "Reveal the active file",
        "Selects the file of the tab you switch to in the Files panel, opening the folders it is in.",
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
    // ---- search ----
    field(
        "panels.search",
        "case_sensitive",
        "Match case",
        "Finds only text with the same upper and lower case. Off, a query with an upper-case letter still matches case exactly. The Search panel's Aa button switches it too.",
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
        "On by default, as in Zed: copies and deletes in Vim mode also go to the system clipboard, and p pastes from it. Off, as in Vim itself, they use Vim's own registers (a private one in each field).",
    ),
    field(
        "editor",
        "vim_mappings.insert",
        "Vim mappings in Insert mode",
        "Your own keys for Insert mode, as Vim's inoremap: keys you type, and the keys Vim gets instead, such as jk for <Esc>. Edit them in settings.toml; the Keyboard page lists them with Vim's own keys.",
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
        "How long a mapping of several keys waits for the next one, as Vim's timeoutlen. After that the keys typed so far count as themselves: a lone j types j.",
    ),
    field(
        "editor",
        "font_size",
        "Code font size",
        "Font size of code in file tabs, in points. The font is the color pack's monospace family.",
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
        "File tabs tint the lines an agent's current turn changed, mark them in the gutter, and let you reject one change at a time. Off, nothing is compared or drawn; agents still write the files as before.",
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

/// The title of `namespace` in the sheet.
pub fn namespace_title(namespace: &str) -> String {
    NAMESPACES
        .iter()
        .find(|(ns, _)| *ns == namespace)
        .map_or_else(|| humanize(namespace), |(_, title)| (*title).to_owned())
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
        "notifications" => {
            "Notify notifications alerts native notification centre Dock badge sound toast \
             requests failures finished ready for review"
        }
        "sessions_restore" => {
            "Restore sessions restart respawn home folder environment login shell projects"
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
        SEC_OMP = "OMP";
        SEC_CLAUDE = "Claude Code";
        SEC_CODEX = "Codex";
        SEC_MCP = "MCP servers";
        SEC_LIMITS = "Limits";
        SEC_FINISH = "Finish";
        SEC_CHECKPOINTS = "Checkpoints";
        SEC_CHECKPOINTS_HELP = "What an agent chat's turns record, so their files can be rewound (Rewind… in a turn's footer).";
        SEC_CHECKPOINTS_KEPT = "Checkpoints kept";
        CHECKPOINT_PROJECTS = "A project can differ: [checkpoints.projects.<name or id>] in daemon.toml sets its mode and the four snapshot bounds.";
        CHECKPOINT_CHATS_NOT_ON = "Chats without checkpoints now:";
        SEC_ASSISTANT_PANEL = "Mawas";
        SEC_ASSISTANT_ACT = "What Mawas may do";
        SEC_ASSISTANT_ACT_HELP = "Ask before acting: Mawas can change things, and every change asks you first (grants answer for what you allowed). Advisor: Mawas reads and advises, and cannot change anything. A change of mode applies from the next message.";
        SEC_ASSISTANT_GRANTS = "What you allowed Mawas to do without asking";
        SEC_ASSISTANT_GRANTS_HELP = "Granted from an approval card: for one chat, or always in a workspace. Destructive changes always ask, whatever is granted.";
        GRANTS_NONE = "Nothing is granted: Mawas asks before every change.";
        GRANT_REVOKE = "Revoke";
        GRANT_REVOKE_ALL = "Revoke all";
        GRANTS_UNSUPPORTED = "This hutand is older than the app and has no grants: restart it (Daemon → Restart).";
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
        PROVIDERS_NONE = "No model provider is set up: add one on the Models page.";
        NOT_OPENED = "Not opened yet: Mawas starts on its backend the first time you use it.";
        NO_PROJECT_NOTE = "Open a project to set its own backend and permissions here.";
        NO_MATCH_HINT = "Clear the search to see every page, or try another word.";
        NO_MATCH_FOOTER = "No matches";
        CLEAR_SEARCH = "Clear search";
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
        PROBLEMS_NONE = "Problems · none";
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
    use crate::workbench::settings_schema::{FieldKind, catalog, fields_of, namespace_schema};
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
        for entry in catalog() {
            let ns = entry.namespace;
            assert!(
                NAMESPACES.iter().any(|(n, _)| *n == ns),
                "namespace {ns} has no title"
            );
            assert_user_facing(&format!("namespace {ns}"), &namespace_title(ns));
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
        for page in ["data", "models", "accounts", "history", "integrations"] {
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
        ];
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
