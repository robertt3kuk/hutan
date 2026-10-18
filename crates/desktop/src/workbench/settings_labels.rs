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
    ("panels.inspector", "Inspector"),
    ("panels.overview", "Overview"),
    ("workspaces", "Workspaces"),
    ("history", "History"),
    ("chat", "Chat"),
    ("editor", "Editing"),
    ("agents", "Agents"),
    ("resources", "Resources"),
    ("tasks", "Tasks"),
    ("assistant", "Assistant"),
    ("integrations", "Agent Integrations"),
    ("launcher", "Start Sheet"),
    ("accounts", "Accounts"),
    ("models", "Models"),
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
        "First-run setup finished",
        "Whether the first-run setup was completed or skipped. Turn it off to see the setup again at the next start, or use “Run setup again” below.",
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
        "A menu bar icon with the daemon's state, how many sessions need you, and a menu to jump to them. While no Hutan window is connected, the daemon shows its own icon whatever this says.",
    ),
    field(
        "app",
        "dock_badge",
        "Show a badge on the Dock icon",
        "Shows how many sessions need you (blocked, waiting for an approval, failed or unread) on the Dock icon.",
    ),
    // ---- keyboard ----
    choice(
        "keymap",
        "base",
        "Key bindings",
        "The set of shortcuts Hutan starts from. Zed keys follow Zed on macOS (⌘⇧P command palette, ⌘P jump, ⌘B ⌘R ⌘J docks); Hutan keys are the original ones (⌘K command palette, ⌘J jump). Your own bindings in keymap.toml are applied on top, and a change takes effect at once.",
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
        "The terminal's colors. Automatic follows the system light or dark mode; the others keep one theme whatever the system does.",
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
            ("overview", "Overview"),
        ],
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
        "“Needs you” section",
        "Shows a section on top with the sessions that are waiting for you.",
    ),
    choice(
        "panels.sessions",
        "row",
        "Row contents",
        "What each row shows, in order. The first line can show state, kind, title and age; the second line can show folder (where the session is now) and placement (main checkout or worktree branch). Pick the parts from the menu.",
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
    // ---- overview ----
    choice(
        "panels.overview",
        "card_size",
        "Card size",
        "How large each session's card is on the overview board.",
        &[("small", "Small"), ("medium", "Medium"), ("large", "Large")],
    ),
    choice(
        "panels.overview",
        "preview",
        "Card preview",
        "What a card shows of the terminal: a live view, a still picture, or nothing.",
        &[("live", "Live"), ("static", "Still"), ("off", "Off")],
    ),
    choice(
        "panels.overview",
        "sort",
        "Order cards by",
        "How the cards are ordered.",
        &[
            ("attention", "Needs attention first"),
            ("recent", "Most recent"),
            ("name", "Name"),
        ],
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
    field(
        "chat",
        "send_on_enter",
        "Send with Return",
        "Return sends the message and Shift-Return starts a new line. Off, Return starts a new line and ⌘-Return sends.",
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
        "Vim-style editing (Normal, Insert and Visual modes) in Hutan's text fields. Fields start in Insert mode; Escape switches to Normal, and Escape again does what it always did, such as closing a dialog. Terminals are not affected.",
    ),
    field(
        "editor",
        "vim_use_system_clipboard",
        "Vim uses the system clipboard",
        "Copies and deletes in Vim mode also go to the system clipboard, and p pastes from it. Off, they use a private register in each field.",
    ),
    // ---- agents ----
    field(
        "agents",
        "show_finished",
        "Show finished agents",
        "Lists agents that have finished, dimmed, with their sub-agents folded into one row. Off, only agents still around are listed.",
    ),
    field(
        "agents",
        "only_blocked",
        "Only blocked agents",
        "Lists only the agents that are blocked, plus the ones above them.",
    ),
    choice(
        "agents",
        "scope",
        "Show agents from",
        "Whether the monitor lists agents of the current workspace or of every workspace.",
        &[
            ("active_workspace", "The current workspace"),
            ("all", "All workspaces"),
        ],
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
        "Use the assistant",
        "Off hides the Assistant panel, removes its commands and shortcuts, and releases the system-wide shortcut. Your earlier assistant conversations are kept.",
    ),
    field(
        "assistant",
        "global_shortcut",
        "System-wide shortcut",
        "A shortcut that brings Hutan forward and opens the Assistant from any app, for example alt-space or cmd-shift-space. It needs at least one of cmd, ctrl or alt. Leave it empty to turn it off. If another app already uses it, Hutan tells you once and leaves the setting alone.",
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
        "history" => "Index sessions indexing on off sources index size",
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
        RESET = "Reset";
        RESET_PAGE = "Reset page…";
        RESET_PAGE_TITLE = "Reset this page?";
        RESET_PAGE_CONFIRM = "Reset page";
        RESET_PAGE_NOTHING = "Nothing on this page is set in the file it edits";
        ADD_CHOICE = "+ add ▾";
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
    }

    /// "Workspace api", or just "Workspace" when the container's name is not known.
    pub fn layer_named(layer: &str, name: Option<&String>) -> String {
        match name {
            Some(name) => format!("{layer} {name}"),
            None => layer.to_owned(),
        }
    }

    /// "editing Project api": the layer a change to a field goes to.
    pub fn editing(layer: &str) -> String {
        format!("editing {layer}")
    }

    /// A value the app does not offer.
    pub fn not_available(value: &str) -> String {
        format!("{value} (not available)")
    }

    pub fn range_hint(low: f64, high: f64) -> String {
        format!("{} to {}", number(low), number(high))
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
                "The daemon's configuration. It is read when the daemon starts.",
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
    fn the_checker_rejects_what_it_is_for() {
        assert!(has_snake_case("see on_last_window_close"));
        assert!(!has_snake_case("Font size, in points."));
    }
}
