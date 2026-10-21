//! Which language servers exist and which one serves a file: the built-in table merged with
//! `[lsp.servers.<id>]` of `daemon.toml`, and the language of a file name.
//!
//! Servers are **found, never fetched** (ADR 0012 amendment point 5): the table names the
//! command to look for on the login `PATH` and the command that installs it, which is shown and
//! never run.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::config::{LspServerSettings, LspSettings};

/// One server Hutan knows how to run.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerSpec {
    /// Its id: the key of `[lsp.servers.<id>]`.
    pub name: String,
    /// A program name for the login `PATH`, or an absolute path.
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// LSP language ids it serves.
    pub languages: Vec<String>,
    /// Names whose topmost occurrence above a file is the root.
    pub root_markers: Vec<String>,
    pub initialization_options: Option<Value>,
    pub settings: Option<Value>,
    /// `None`: the `[lsp]` default.
    pub idle_stop_secs: Option<u64>,
    /// The command that installs the server, shown when it is missing.
    pub install: Option<String>,
    pub max_failures: Option<u32>,
    pub failure_window: Option<Duration>,
    pub max_documents: Option<usize>,
}

impl ServerSpec {
    /// The setting a user changes to point at a binary Hutan did not find.
    pub fn command_setting(&self) -> String {
        format!("lsp.servers.{}.command", self.name)
    }
}

struct Builtin {
    name: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    languages: &'static [&'static str],
    root_markers: &'static [&'static str],
    install: &'static str,
}

/// Searched in this order: the first installed one of a language serves it.
const BUILTIN: &[Builtin] = &[
    Builtin {
        name: "rust-analyzer",
        command: "rust-analyzer",
        args: &[],
        languages: &["rust"],
        root_markers: &["Cargo.toml", "rust-project.json"],
        install: "rustup component add rust-analyzer",
    },
    Builtin {
        name: "typescript-language-server",
        command: "typescript-language-server",
        args: &["--stdio"],
        languages: &[
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ],
        root_markers: &["tsconfig.json", "jsconfig.json", "package.json"],
        install: "npm install -g typescript-language-server typescript",
    },
    Builtin {
        name: "pyright",
        command: "pyright-langserver",
        args: &["--stdio"],
        languages: &["python"],
        root_markers: &[
            "pyrightconfig.json",
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
        install: "npm install -g pyright",
    },
    Builtin {
        name: "basedpyright",
        command: "basedpyright-langserver",
        args: &["--stdio"],
        languages: &["python"],
        root_markers: &[
            "pyrightconfig.json",
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
        ],
        install: "pip install basedpyright",
    },
    Builtin {
        name: "gopls",
        command: "gopls",
        args: &[],
        languages: &["go", "gomod"],
        root_markers: &["go.work", "go.mod"],
        install: "go install golang.org/x/tools/gopls@latest",
    },
    Builtin {
        name: "clangd",
        command: "clangd",
        args: &[],
        languages: &["c", "cpp", "objective-c", "objective-cpp"],
        root_markers: &["compile_commands.json", "compile_flags.txt", ".clangd"],
        install: "brew install llvm",
    },
    Builtin {
        name: "bash-language-server",
        command: "bash-language-server",
        args: &["start"],
        languages: &["shellscript"],
        root_markers: &[],
        install: "npm install -g bash-language-server",
    },
    Builtin {
        name: "vscode-json-languageserver",
        command: "vscode-json-language-server",
        args: &["--stdio"],
        languages: &["json", "jsonc"],
        root_markers: &[],
        install: "npm install -g vscode-langservers-extracted",
    },
    Builtin {
        name: "taplo",
        command: "taplo",
        args: &["lsp", "stdio"],
        languages: &["toml"],
        root_markers: &[],
        install: "cargo install taplo-cli --locked --features lsp",
    },
    Builtin {
        name: "marksman",
        command: "marksman",
        args: &["server"],
        languages: &["markdown"],
        root_markers: &[".marksman.toml"],
        install: "brew install marksman",
    },
];

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn merged(
    name: &str,
    builtin: Option<&Builtin>,
    over: &LspServerSettings,
) -> Result<ServerSpec, String> {
    let command = over
        .command
        .clone()
        .or_else(|| builtin.map(|b| b.command.to_owned()))
        .ok_or_else(|| format!("[lsp.servers.{name}] needs a `command`"))?;
    let languages = over
        .languages
        .clone()
        .or_else(|| builtin.map(|b| owned(b.languages)))
        .filter(|l| !l.is_empty())
        .ok_or_else(|| format!("[lsp.servers.{name}] needs `languages`"))?;
    // Another command than the built-in one: its arguments are the user's too.
    let default_args = match (&over.command, builtin) {
        (None, Some(b)) => owned(b.args),
        _ => Vec::new(),
    };
    Ok(ServerSpec {
        name: name.to_owned(),
        command,
        args: over.args.clone().unwrap_or(default_args),
        env: over.env.clone(),
        languages,
        root_markers: over
            .root_markers
            .clone()
            .or_else(|| builtin.map(|b| owned(b.root_markers)))
            .unwrap_or_default(),
        initialization_options: over.initialization_options.clone(),
        settings: over.settings.clone(),
        idle_stop_secs: over.idle_stop_secs,
        install: over
            .install
            .clone()
            .or_else(|| builtin.map(|b| b.install.to_owned())),
        max_failures: over.max_failures,
        failure_window: over.failure_window_secs.map(Duration::from_secs),
        max_documents: over.max_documents,
    })
}

/// The servers a daemon knows, in the order they are tried: the user's own first (in the order
/// of their ids), then the built-in ones, minus the disabled.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Catalog {
    specs: Vec<ServerSpec>,
}

impl Catalog {
    pub fn new(settings: &LspSettings) -> Result<Self, String> {
        let mut specs = Vec::new();
        for (name, over) in &settings.servers {
            if BUILTIN.iter().any(|b| b.name == name) || over.enabled == Some(false) {
                continue;
            }
            specs.push(merged(name, None, over)?);
        }
        for builtin in BUILTIN {
            let over = settings
                .servers
                .get(builtin.name)
                .cloned()
                .unwrap_or_default();
            if over.enabled == Some(false) {
                continue;
            }
            specs.push(merged(builtin.name, Some(builtin), &over)?);
        }
        Ok(Self { specs })
    }

    /// The servers that serve `language`, in the order they are tried.
    pub fn for_language<'a>(&'a self, language: &'a str) -> impl Iterator<Item = &'a ServerSpec> {
        self.specs
            .iter()
            .filter(move |spec| spec.languages.iter().any(|l| l == language))
    }

    #[cfg(test)]
    pub fn get(&self, name: &str) -> Option<&ServerSpec> {
        self.specs.iter().find(|spec| spec.name == name)
    }
}

/// The LSP language id of a file, from its name; `None` for a file no language is known for.
pub fn language_of(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    match name {
        "Cargo.lock" | "Pipfile" => return Some("toml"),
        "Makefile" | "makefile" | "GNUmakefile" => return Some("makefile"),
        "Dockerfile" => return Some("dockerfile"),
        ".bashrc" | ".bash_profile" | ".zshrc" | ".zprofile" | ".profile" | ".zshenv" => {
            return Some("shellscript");
        }
        _ => {}
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" | "pyw" => "python",
        "go" => "go",
        "mod" if name == "go.mod" => "gomod",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "ipp" => "cpp",
        "m" => "objective-c",
        "mm" => "objective-cpp",
        "sh" | "bash" | "zsh" => "shellscript",
        "json" => "json",
        "jsonc" => "jsonc",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" => "css",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_servers_are_searched_in_order_and_python_has_two() {
        let catalog = Catalog::new(&LspSettings::default()).unwrap();
        let rust: Vec<&str> = catalog
            .for_language("rust")
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(rust, ["rust-analyzer"]);
        let python: Vec<&str> = catalog
            .for_language("python")
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(python, ["pyright", "basedpyright"]);
        let ra = catalog.get("rust-analyzer").unwrap();
        assert_eq!(ra.root_markers, ["Cargo.toml", "rust-project.json"]);
        assert_eq!(
            ra.install.as_deref(),
            Some("rustup component add rust-analyzer")
        );
        assert_eq!(ra.command_setting(), "lsp.servers.rust-analyzer.command");
        assert!(catalog.for_language("brainfuck").next().is_none());
    }

    #[test]
    fn settings_override_a_built_in_server_and_add_servers_of_their_own() {
        let settings = crate::config::DaemonSettings::parse(
            r#"
[lsp.servers.rust-analyzer]
command = "/opt/ra"
args = ["--log-file", "/tmp/ra.log"]
env = { RA_LOG = "warn" }
install = "brew install rust-analyzer"

[lsp.servers.gopls]
enabled = false

[lsp.servers.my-lang]
command = "my-lsp"
languages = ["mylang"]
root_markers = ["my.toml"]
"#,
            None,
        )
        .unwrap()
        .lsp;
        let catalog = Catalog::new(&settings).unwrap();
        let ra = catalog.get("rust-analyzer").unwrap();
        assert_eq!(ra.command, "/opt/ra");
        assert_eq!(ra.args, ["--log-file", "/tmp/ra.log"]);
        assert_eq!(ra.env["RA_LOG"], "warn");
        assert_eq!(ra.languages, ["rust"]);
        assert_eq!(ra.install.as_deref(), Some("brew install rust-analyzer"));
        assert!(catalog.get("gopls").is_none());
        let mine = catalog.for_language("mylang").next().unwrap();
        assert_eq!(mine.name, "my-lang");
        assert_eq!(mine.root_markers, ["my.toml"]);
        assert_eq!(mine.install, None);
        // The user's own servers come before the built-in ones.
        let mut with_rust = settings.clone();
        with_rust.servers.get_mut("my-lang").unwrap().languages = Some(vec!["rust".to_owned()]);
        let catalog = Catalog::new(&with_rust).unwrap();
        let rust: Vec<&str> = catalog
            .for_language("rust")
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(rust, ["my-lang", "rust-analyzer"]);
    }

    #[test]
    fn a_server_of_your_own_needs_a_command_and_languages() {
        let mut settings = LspSettings::default();
        settings
            .servers
            .insert("mine".to_owned(), LspServerSettings::default());
        assert!(Catalog::new(&settings).is_err());
        settings.servers.get_mut("mine").unwrap().command = Some("mine".to_owned());
        assert!(Catalog::new(&settings).is_err());
        settings.servers.get_mut("mine").unwrap().languages = Some(vec!["x".to_owned()]);
        assert!(Catalog::new(&settings).is_ok());
    }

    #[test]
    fn languages_come_from_the_file_name() {
        for (path, language) in [
            ("/r/src/main.rs", Some("rust")),
            ("/r/web/app.tsx", Some("typescriptreact")),
            ("/r/web/app.MTS", Some("typescript")),
            ("/r/tool.py", Some("python")),
            ("/r/go.mod", Some("gomod")),
            ("/r/include/a.hpp", Some("cpp")),
            ("/r/run.sh", Some("shellscript")),
            ("/r/Cargo.lock", Some("toml")),
            ("/r/notes.txt", None),
            ("/r/noextension", None),
        ] {
            assert_eq!(language_of(Path::new(path)), language, "{path}");
        }
    }
}
