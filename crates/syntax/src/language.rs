//! The language registry: which grammars exist, which are compiled in, and how a file or a
//! markdown fence label maps to one.

use std::fmt;
use std::path::Path;

/// A language Hutan can parse. Every variant exists whatever the cargo features; whether its
/// grammar is linked is [`Language::is_compiled`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    /// Rust (`.rs`).
    Rust,
    /// TypeScript (`.ts`, `.mts`, `.cts`).
    TypeScript,
    /// TypeScript with JSX (`.tsx`).
    Tsx,
    /// JavaScript and JSX (`.js`, `.mjs`, `.cjs`, `.jsx`).
    JavaScript,
    /// Python (`.py`, `.pyi`).
    Python,
    /// Go (`.go`).
    Go,
    /// JSON, with comments (`.json`, `.jsonc`).
    Json,
    /// TOML (`.toml`, `Cargo.lock`).
    Toml,
    /// YAML (`.yaml`, `.yml`).
    Yaml,
    /// Markdown, block and inline structure (`.md`, `.markdown`).
    Markdown,
    /// Bash and compatible shells, including zsh (`.sh`, `.bash`, `.zsh`).
    Bash,
    /// HTML (`.html`, `.htm`).
    Html,
    /// CSS (`.css`).
    Css,
    /// C (`.c`, `.h`).
    C,
    /// C++ (`.cc`, `.cpp`, `.cxx`, `.hpp`, `.hh`, `.hxx`).
    Cpp,
}

impl Language {
    /// Every language, compiled in or not.
    pub const ALL: [Language; 15] = [
        Language::Rust,
        Language::TypeScript,
        Language::Tsx,
        Language::JavaScript,
        Language::Python,
        Language::Go,
        Language::Json,
        Language::Toml,
        Language::Yaml,
        Language::Markdown,
        Language::Bash,
        Language::Html,
        Language::Css,
        Language::C,
        Language::Cpp,
    ];

    /// The languages whose grammar is linked into this build.
    pub fn compiled() -> impl Iterator<Item = Language> {
        Self::ALL.into_iter().filter(|l| l.is_compiled())
    }

    /// The stable lowercase name (`rust`, `tsx`, `cpp`): what settings (`languages.<name>`) and
    /// markdown fences use.
    pub const fn name(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Json => "json",
            Language::Toml => "toml",
            Language::Yaml => "yaml",
            Language::Markdown => "markdown",
            Language::Bash => "bash",
            Language::Html => "html",
            Language::Css => "css",
            Language::C => "c",
            Language::Cpp => "cpp",
        }
    }

    /// The cargo feature of this crate that links the grammar (TSX shares TypeScript's).
    pub const fn feature(self) -> &'static str {
        match self {
            Language::Tsx | Language::TypeScript => "typescript",
            other => other.name(),
        }
    }

    /// Whether the grammar is linked into this build.
    pub const fn is_compiled(self) -> bool {
        match self {
            Language::Rust => cfg!(feature = "rust"),
            Language::TypeScript | Language::Tsx => cfg!(feature = "typescript"),
            Language::JavaScript => cfg!(feature = "javascript"),
            Language::Python => cfg!(feature = "python"),
            Language::Go => cfg!(feature = "go"),
            Language::Json => cfg!(feature = "json"),
            Language::Toml => cfg!(feature = "toml"),
            Language::Yaml => cfg!(feature = "yaml"),
            Language::Markdown => cfg!(feature = "markdown"),
            Language::Bash => cfg!(feature = "bash"),
            Language::Html => cfg!(feature = "html"),
            Language::Css => cfg!(feature = "css"),
            Language::C => cfg!(feature = "c"),
            Language::Cpp => cfg!(feature = "cpp"),
        }
    }

    /// Looks a language up by [`name`](Self::name) or a common alias (`rs`, `ts`, `py`, `sh`,
    /// `yml`, `c++`, `golang`, `jsonc`, …), ignoring case: what follows ```` ``` ```` in markdown.
    pub fn from_name(name: &str) -> Option<Language> {
        let lower = name.trim().to_ascii_lowercase();
        Some(match lower.as_str() {
            "rust" | "rs" => Language::Rust,
            "typescript" | "ts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "javascript" | "js" | "jsx" | "node" | "mjs" | "cjs" => Language::JavaScript,
            "python" | "py" | "python3" => Language::Python,
            "go" | "golang" => Language::Go,
            "json" | "jsonc" => Language::Json,
            "toml" => Language::Toml,
            "yaml" | "yml" => Language::Yaml,
            "markdown" | "md" => Language::Markdown,
            "bash" | "sh" | "shell" | "zsh" | "shellscript" => Language::Bash,
            "html" | "htm" => Language::Html,
            "css" => Language::Css,
            "c" => Language::C,
            "cpp" | "c++" | "cxx" | "cc" => Language::Cpp,
            _ => return None,
        })
    }

    /// The language of a file extension (without the dot), ignoring case.
    pub fn from_extension(extension: &str) -> Option<Language> {
        let lower = extension.to_ascii_lowercase();
        Some(match lower.as_str() {
            "rs" => Language::Rust,
            "ts" | "mts" | "cts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Language::JavaScript,
            "py" | "pyi" => Language::Python,
            "go" => Language::Go,
            "json" | "jsonc" => Language::Json,
            "toml" => Language::Toml,
            "yaml" | "yml" => Language::Yaml,
            "md" | "markdown" => Language::Markdown,
            "sh" | "bash" | "zsh" => Language::Bash,
            "html" | "htm" => Language::Html,
            "css" => Language::Css,
            "c" | "h" => Language::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "c++" | "h++" => Language::Cpp,
            _ => return None,
        })
    }

    /// The language of a path: well-known file names first (`Cargo.lock`, `.zshrc`), then the
    /// extension.
    pub fn from_path(path: &Path) -> Option<Language> {
        let file_name = path.file_name()?.to_str()?;
        match file_name {
            "Cargo.lock" | "Pipfile" | "poetry.lock" => return Some(Language::Toml),
            ".bashrc" | ".bash_profile" | ".bash_login" | ".bash_logout" | ".profile"
            | ".zshrc" | ".zshenv" | ".zprofile" | ".zlogin" | "PKGBUILD" => {
                return Some(Language::Bash);
            }
            ".babelrc" | ".eslintrc" | ".prettierrc" | ".swcrc" | "tsconfig.json" => {
                return Some(Language::Json);
            }
            _ => {}
        }
        Self::from_extension(path.extension()?.to_str()?)
    }

    /// The language a `#!` line names (`#!/usr/bin/env python3`, `#!/bin/bash`), for files
    /// without a telling extension. `first_line` is the file's first line.
    pub fn from_shebang(first_line: &str) -> Option<Language> {
        let rest = first_line.strip_prefix("#!")?.trim();
        let mut words = rest.split_whitespace();
        let mut program = words.next()?.rsplit('/').next()?;
        if program == "env" {
            // `env -S node --flag`: the first word that is not an option or an assignment.
            program = words.find(|w| !w.starts_with('-') && !w.contains('='))?;
        }
        let program = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        match program {
            "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" => Some(Language::Bash),
            "python" | "pypy" => Some(Language::Python),
            "node" | "nodejs" | "deno" | "bun" => Some(Language::JavaScript),
            _ => None,
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for language in Language::ALL {
            assert_eq!(Language::from_name(language.name()), Some(language));
        }
    }

    #[test]
    fn paths_and_extensions() {
        let cases = [
            ("src/main.rs", Language::Rust),
            ("a/b.TS", Language::TypeScript),
            ("x.tsx", Language::Tsx),
            ("Cargo.lock", Language::Toml),
            (".zshrc", Language::Bash),
            ("include/x.hpp", Language::Cpp),
            ("include/x.h", Language::C),
            ("docs/README.md", Language::Markdown),
            ("ci.yml", Language::Yaml),
        ];
        for (path, want) in cases {
            assert_eq!(Language::from_path(Path::new(path)), Some(want), "{path}");
        }
        assert_eq!(Language::from_path(Path::new("Makefile")), None);
        assert_eq!(Language::from_path(Path::new("noext")), None);
    }

    #[test]
    fn shebangs() {
        assert_eq!(Language::from_shebang("#!/bin/bash"), Some(Language::Bash));
        assert_eq!(
            Language::from_shebang("#!/usr/bin/env python3"),
            Some(Language::Python)
        );
        assert_eq!(
            Language::from_shebang("#!/usr/bin/env -S node --flag"),
            Some(Language::JavaScript)
        );
        assert_eq!(Language::from_shebang("#!/usr/bin/perl"), None);
        assert_eq!(Language::from_shebang("not a shebang"), None);
    }
}
