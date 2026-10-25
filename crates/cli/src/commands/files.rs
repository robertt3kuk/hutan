//! `hutan fs ls|cat|stat|write|find|search` and `hutan open`: files through the daemon
//! (protocol 0.33, `docs/protocol.md` § Files).

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use hutan_client::Client;
use hutan_protocol::fs::{FileVersion, FsEntry, FsFind, FsKind, FsList, FsSearch, FsWriteBegin};
use serde_json::json;

use super::Ctx;
use crate::args::{
    FsCommand, FsFindArgs, FsLsArgs, FsPathArgs, FsSearchArgs, FsWriteArgs, OpenArgs,
};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn fs(&self, command: FsCommand) -> Result<(), CliError> {
        let client = self.fs_client().await?;
        match command {
            FsCommand::Ls(args) => self.fs_ls(&client, args).await,
            FsCommand::Cat(args) => self.fs_cat(&client, args).await,
            FsCommand::Stat(args) => self.fs_stat(&client, args).await,
            FsCommand::Write(args) => self.fs_write(&client, args).await,
            FsCommand::Find(args) => self.fs_find(&client, args).await,
            FsCommand::Search(args) => self.fs_search(&client, args).await,
        }
    }

    /// `hutan open path[:line[:col]]`: the daemon looks the file up (so the same confinement
    /// applies as everywhere), then the window that was active last shows it in a file tab at
    /// the line (`ui.run workbench::OpenFile`). Without a window the command only prints where
    /// the file is.
    pub(super) async fn open(&self, args: OpenArgs) -> Result<(), CliError> {
        let client = self.fs_client().await?;
        let target = parse_target(&args.target);
        let entry = client
            .fs_stat(&absolute(&target.path)?, false)
            .await
            .map_err(|e| self.fail(e))?;
        if entry.kind != FsKind::File && target.line.is_some() {
            return Err(CliError::Failed(format!(
                "{} is not a file, so it has no line {}",
                entry.path.display(),
                target.line.unwrap_or_default()
            )));
        }
        let shown = match (target.line, target.column) {
            (Some(line), Some(column)) => format!("{}:{line}:{column}", entry.path.display()),
            (Some(line), None) => format!("{}:{line}", entry.path.display()),
            _ => entry.path.display().to_string(),
        };
        let mut opened = false;
        if entry.kind == FsKind::File && client.supports_control() {
            let mut run_args = json!({ "path": entry.path });
            if let Some(line) = target.line {
                run_args["line"] = json!(line);
            }
            if let Some(column) = target.column {
                run_args["column"] = json!(column);
            }
            match client
                .ui_run(hutan_protocol::ui::UiRun {
                    window_id: None,
                    action: "workbench::OpenFile".to_owned(),
                    args: Some(run_args),
                })
                .await
            {
                Ok(_) => opened = true,
                // No window is open: say where the file is, as before. A window that refuses
                // is an error, not a path on stdout.
                Err(error) if super::ui::no_window(&error) => {}
                Err(error) => return Err(self.fail(error)),
            }
        }
        self.print(
            &json!({
                "path": entry.path,
                "kind": entry.kind,
                "line": target.line,
                "column": target.column,
                "opened": opened,
            }),
            || {
                if opened {
                    format!("opened {shown} in Hutan")
                } else {
                    shown.clone()
                }
            },
        )
    }

    async fn fs_client(&self) -> Result<Client, CliError> {
        let client = self.connect().await?;
        if !client.supports_fs() {
            return Err(CliError::Failed(
                "this hutand has no file service (it is older than protocol 0.33); restart it \
                 with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        Ok(client)
    }

    async fn fs_ls(&self, client: &Client, args: FsLsArgs) -> Result<(), CliError> {
        let dir = match args.path {
            Some(path) => absolute(&path)?,
            None => current_dir()?,
        };
        let listing = client
            .fs_list(FsList {
                dir,
                ignore: !args.no_ignore,
                limit: args.limit,
                picked: args.picked,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&listing, || {
            let mut text = entry_table(&listing.entries);
            if listing.truncated {
                text.push_str("\n… more entries than shown");
            }
            text
        })
    }

    async fn fs_cat(&self, client: &Client, args: FsPathArgs) -> Result<(), CliError> {
        let file = client
            .fs_read_all(&absolute(&args.path)?, args.picked)
            .await
            .map_err(|e| self.fail(e))?;
        if self.json {
            return self.print(
                &json!({
                    "path": file.path,
                    "text": file.text,
                    "version": file.version,
                    "line_ending": file.line_ending,
                    "bom": file.bom,
                }),
                String::new,
            );
        }
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(file.text.as_bytes())?;
        stdout.flush()?;
        Ok(())
    }

    async fn fs_stat(&self, client: &Client, args: FsPathArgs) -> Result<(), CliError> {
        let entry = client
            .fs_stat(&absolute(&args.path)?, args.picked)
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&entry, || {
            format!(
                "{}\nkind    {}\nsize    {}\nversion {}",
                entry.path.display(),
                kind_name(entry.kind),
                entry.size,
                version_text(&entry.version()),
            )
        })
    }

    async fn fs_write(&self, client: &Client, args: FsWriteArgs) -> Result<(), CliError> {
        let expect = args.expect.as_deref().map(parse_version).transpose()?;
        let text = match (&args.text, &args.from) {
            (Some(text), _) => text.clone(),
            (None, Some(file)) if file != Path::new("-") => std::fs::read_to_string(file)
                .map_err(|e| CliError::Failed(format!("reading {}: {e}", file.display())))?,
            _ => {
                let mut text = String::new();
                std::io::stdin()
                    .lock()
                    .read_to_string(&mut text)
                    .map_err(|e| CliError::Failed(format!("reading stdin: {e}")))?;
                text
            }
        };
        let written = client
            .fs_write(
                FsWriteBegin {
                    path: absolute(&args.path)?,
                    expect,
                    create: args.create,
                    size_hint: None,
                    picked: args.picked,
                },
                &text,
            )
            .await
            .map_err(|error| {
                // What a later `--expect` needs to try again after reading the file anew.
                if let Some(current) = error.conflict_current() {
                    eprintln!("current version: {}", version_text(&current));
                }
                self.fail(error)
            })?;
        self.print(&written, || {
            format!(
                "wrote {} ({} bytes), version {}",
                written.path.display(),
                written.version.size,
                version_text(&written.version)
            )
        })
    }

    async fn fs_find(&self, client: &Client, args: FsFindArgs) -> Result<(), CliError> {
        let root = match args.root {
            Some(root) => absolute(&root)?,
            None => current_dir()?,
        };
        let found = client
            .fs_find(FsFind {
                root,
                query: args.query,
                limit: args.limit,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&found, || {
            found
                .matches
                .iter()
                .map(|m| m.path.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    async fn fs_search(&self, client: &Client, args: FsSearchArgs) -> Result<(), CliError> {
        let root = match args.root {
            Some(root) => absolute(&root)?,
            None => current_dir()?,
        };
        let case_sensitive = match (args.ignore_case, args.case_sensitive) {
            (true, _) => Some(false),
            (_, true) => Some(true),
            _ => None,
        };
        let result = client
            .fs_search(FsSearch {
                search_id: format!("cli-{}", std::process::id()),
                root,
                pattern: args.pattern,
                regex: args.regex,
                case_sensitive,
                globs: args.globs,
                limit: args.limit,
            })
            .await
            .map_err(|e| self.fail(e))?;
        if !self.json {
            let mut stdout = std::io::stdout().lock();
            for hit in &result.hits {
                writeln!(stdout, "{}:{}:{}", hit.path, hit.line, hit.text)?;
            }
            stdout.flush()?;
            if result.truncated {
                eprintln!(
                    "more hits than the limit: showing {} (narrow the pattern or raise --limit)",
                    result.hits.len()
                );
            }
            return Ok(());
        }
        self.print(&result, String::new)
    }
}

/// `path[:line[:col]]`; a file whose name ends in `:digits` is taken as it is when it exists.
#[derive(Debug, PartialEq, Eq)]
struct Target {
    path: PathBuf,
    line: Option<u32>,
    column: Option<u32>,
}

fn parse_target(text: &str) -> Target {
    let whole = |path: &str| Target {
        path: PathBuf::from(path),
        line: None,
        column: None,
    };
    if Path::new(text).exists() {
        return whole(text);
    }
    let number = |part: &str| {
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u32>().ok())
            .flatten()
    };
    if let Some((rest, last)) = text.rsplit_once(':')
        && let Some(last) = number(last)
    {
        if let Some((path, middle)) = rest.rsplit_once(':')
            && let Some(line) = number(middle)
        {
            return Target {
                path: PathBuf::from(path),
                line: Some(line),
                column: Some(last),
            };
        }
        return Target {
            path: PathBuf::from(rest),
            line: Some(last),
            column: None,
        };
    }
    whole(text)
}

fn current_dir() -> Result<PathBuf, CliError> {
    Ok(std::env::current_dir()?)
}

/// The path as the daemon wants it: absolute, relative to the current directory.
fn absolute(path: &Path) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(current_dir()?.join(path))
    }
}

fn version_text(version: &FileVersion) -> String {
    format!("{}:{}:{}", version.mtime_ns, version.size, version.ino)
}

fn parse_version(text: &str) -> Result<FileVersion, CliError> {
    let bad = || {
        CliError::Usage(format!(
            "`{text}` is not a version: expected `mtime_ns:size:ino`, as `hutan fs stat` prints it"
        ))
    };
    let mut parts = text.split(':');
    let (Some(mtime), Some(size), Some(ino), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad());
    };
    Ok(FileVersion {
        mtime_ns: mtime.parse().map_err(|_| bad())?,
        size: size.parse().map_err(|_| bad())?,
        ino: ino.parse().map_err(|_| bad())?,
    })
}

fn kind_name(kind: FsKind) -> &'static str {
    match kind {
        FsKind::File => "file",
        FsKind::Dir => "dir",
        FsKind::Symlink => "symlink",
        FsKind::Other => "other",
    }
}

fn entry_table(entries: &[FsEntry]) -> String {
    if entries.is_empty() {
        return "(empty)".to_owned();
    }
    let sizes: Vec<String> = entries
        .iter()
        .map(|e| match e.kind {
            FsKind::File => e.size.to_string(),
            _ => "-".to_owned(),
        })
        .collect();
    let width = sizes.iter().map(String::len).max().unwrap_or(1);
    entries
        .iter()
        .zip(&sizes)
        .map(|(entry, size)| {
            let mark = match entry.kind {
                FsKind::Dir => 'd',
                FsKind::Symlink => 'l',
                FsKind::File => '-',
                FsKind::Other => '?',
            };
            let slash = if entry.kind == FsKind::Dir { "/" } else { "" };
            let ignored = if entry.ignored { "  (ignored)" } else { "" };
            format!("{mark} {size:>width$}  {}{slash}{ignored}", entry.name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_a_path_with_an_optional_line_and_column() {
        let t = |text| parse_target(text);
        assert_eq!(
            t("src/main.rs"),
            Target {
                path: "src/main.rs".into(),
                line: None,
                column: None
            }
        );
        assert_eq!(
            t("src/main.rs:42"),
            Target {
                path: "src/main.rs".into(),
                line: Some(42),
                column: None
            }
        );
        assert_eq!(
            t("src/main.rs:42:7"),
            Target {
                path: "src/main.rs".into(),
                line: Some(42),
                column: Some(7)
            }
        );
        assert_eq!(t("a:b").line, None);
        assert_eq!(t("a:").path, PathBuf::from("a:"));
    }

    #[test]
    fn a_file_named_with_a_colon_and_digits_is_taken_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let odd = dir.path().join("log:12");
        std::fs::write(&odd, "").unwrap();
        let target = parse_target(odd.to_str().unwrap());
        assert_eq!((target.path, target.line), (odd, None));
    }

    #[test]
    fn versions_round_trip_through_their_text() {
        let version = FileVersion {
            mtime_ns: 1_791_894_150_123_456_789,
            size: 12,
            ino: 99,
        };
        assert_eq!(parse_version(&version_text(&version)).unwrap(), version);
        assert!(parse_version("1:2").is_err());
        assert!(parse_version("1:2:3:4").is_err());
        assert!(parse_version("a:b:c").is_err());
    }
}
