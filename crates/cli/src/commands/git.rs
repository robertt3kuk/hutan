//! `hutan git log|blame|show`: read-only git history through the daemon (protocol 0.47,
//! `docs/protocol.md` § Git history). Thin: the daemon runs git, bounded and read-only; this only
//! prints what it answers.

use std::path::Path;

use hutan_client::Client;
use hutan_protocol::git::{
    GIT_UNCOMMITTED, GitBlame, GitBlameResult, GitCommit, GitFile, GitLog, GitShow,
};
use hutan_protocol::worktree::{DiffFile, DiffStatus};
use serde_json::json;

use super::paths::absolute;
use super::{Ctx, print_line};
use crate::args::{GitBlameArgs, GitCommand, GitLogArgs, GitShowArgs};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn git(&self, command: GitCommand) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_git_history() {
            return Err(CliError::Failed(
                "this hutand cannot answer git history (it is older than protocol 0.47); \
                 restart it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        match command {
            GitCommand::Log(args) => self.git_log(&client, args).await,
            GitCommand::Blame(args) => self.git_blame(&client, args).await,
            GitCommand::Show(args) => self.git_show(&client, args).await,
        }
    }

    async fn git_log(&self, client: &Client, args: GitLogArgs) -> Result<(), CliError> {
        let path = absolute(args.path.as_deref().unwrap_or(Path::new(".")))?;
        let log = client
            .git_log(GitLog {
                path,
                rev: args.rev,
                limit: args.limit,
                before: args.before,
            })
            .await
            .map_err(|e| self.fail(e))?;
        if self.json {
            return self.print(&log, String::new);
        }
        for commit in &log.commits {
            print_line(&log_line(commit))?;
        }
        if let Some(next) = log.next_before.as_ref().filter(|_| log.more) {
            eprintln!("more: continue with --before {next}");
        }
        Ok(())
    }

    async fn git_blame(&self, client: &Client, args: GitBlameArgs) -> Result<(), CliError> {
        let path = absolute(&args.path)?;
        let blame = client
            .git_blame(GitBlame {
                path: path.clone(),
                rev: args.rev.clone(),
                contents: None,
            })
            .await
            .map_err(|e| self.fail(e))?;
        if self.json {
            return self.print(&blame, String::new);
        }
        // The text of the lines: the revision's, or the file in the working tree.
        let text = match &args.rev {
            Some(rev) => {
                client
                    .git_file(GitFile {
                        path,
                        rev: rev.clone(),
                    })
                    .await
                    .map_err(|e| self.fail(e))?
                    .content
            }
            None => std::fs::read_to_string(&path)?,
        };
        for line in blame_lines(&blame, &text) {
            print_line(&line)?;
        }
        if blame.truncated {
            eprintln!(
                "note: the file has more changes of authorship than the daemon lists; the later lines are left out"
            );
        }
        Ok(())
    }

    async fn git_show(&self, client: &Client, args: GitShowArgs) -> Result<(), CliError> {
        let path = absolute(args.path.as_deref().unwrap_or(Path::new(".")))?;
        let shown = client
            .git_show(GitShow {
                path,
                rev: args.rev,
                old_path: None,
                patch: !args.stat,
            })
            .await
            .map_err(|e| self.fail(e))?;
        if self.json {
            return self.print(&json!(shown), String::new);
        }
        print_line(&format!("commit {}", shown.commit.id))?;
        print_line(&format!(
            "Author: {} <{}>",
            shown.commit.author, shown.commit.email
        ))?;
        print_line(&format!(
            "Date:   {}",
            hutan_client::time::local_time_text(shown.commit.time.max(0) as u64 * 1000)
        ))?;
        print_line("")?;
        for line in shown.message.trim_end().lines() {
            print_line(&format!("    {line}"))?;
        }
        print_line("")?;
        if args.stat {
            for file in &shown.files {
                print_line(&stat_line(file))?;
            }
        } else {
            let patch = shown.patch.trim_end_matches('\n');
            if !patch.is_empty() {
                print_line(patch)?;
            }
        }
        if shown.truncated {
            eprintln!("note: the diff is cut (2 MiB) or lists only the first 5000 files");
        }
        Ok(())
    }
}

fn day(time: i64) -> String {
    hutan_client::time::local_time_text(time.max(0) as u64 * 1000)
        .chars()
        .take(10)
        .collect()
}

/// `1a2b3c4 2026-10-25 Ada Lovelace  the subject` (and the old name after a rename).
fn log_line(commit: &GitCommit) -> String {
    let mut line = format!(
        "{} {} {}  {}",
        commit.id.get(..7).unwrap_or(&commit.id),
        day(commit.time),
        commit.author,
        commit.subject
    );
    if commit.merge {
        line.push_str("  (merge)");
    }
    if let Some(old) = &commit.old_path {
        line.push_str(&format!("  (renamed from {old})"));
    }
    line
}

/// One line per line of `text`: `1a2b3c4 Ada Lovelace 2026-10-25   12) text`.
fn blame_lines(blame: &GitBlameResult, text: &str) -> Vec<String> {
    let by_id: std::collections::HashMap<&str, &hutan_protocol::git::BlameCommit> =
        blame.commits.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut owner: Vec<Option<&str>> = Vec::new();
    for hunk in &blame.hunks {
        owner.resize(hunk.line as usize - 1, None);
        owner.extend(std::iter::repeat_n(
            Some(hunk.commit.as_str()),
            hunk.count as usize,
        ));
    }
    let author_width = blame
        .commits
        .iter()
        .map(|c| c.author.chars().count())
        .max()
        .unwrap_or(0)
        .min(24);
    text.lines()
        .enumerate()
        .map(|(at, line)| {
            let id = owner.get(at).copied().flatten();
            let (short, author, when) = match id.and_then(|id| by_id.get(id)) {
                Some(c) if c.id == GIT_UNCOMMITTED => (
                    "0000000".to_owned(),
                    "Not Committed Yet".to_owned(),
                    String::new(),
                ),
                Some(c) => (
                    c.id.get(..7).unwrap_or(&c.id).to_owned(),
                    c.author.clone(),
                    day(c.time),
                ),
                None => ("-------".to_owned(), String::new(), String::new()),
            };
            format!(
                "{short} {author:<width$} {when:<10} {number:>4}) {line}",
                width = author_width.max("Not Committed Yet".len()),
                number = at + 1
            )
        })
        .collect()
}

/// `M  a.txt  +3 -1`.
fn stat_line(file: &DiffFile) -> String {
    let mark = match file.status {
        DiffStatus::Added => "A",
        DiffStatus::Modified => "M",
        DiffStatus::Deleted => "D",
        DiffStatus::Renamed => "R",
    };
    let name = match &file.old_path {
        Some(old) => format!("{old} -> {}", file.path),
        None => file.path.clone(),
    };
    if file.binary {
        format!("{mark}  {name}  binary")
    } else {
        format!("{mark}  {name}  +{} -{}", file.additions, file.deletions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::git::{BlameCommit, BlameHunk};

    fn commit(id: &str, author: &str) -> BlameCommit {
        BlameCommit {
            id: id.to_owned(),
            author: author.to_owned(),
            email: String::new(),
            time: 0,
            subject: String::new(),
            path: "f".into(),
            boundary: false,
        }
    }

    #[test]
    fn blame_prints_one_line_per_line_with_its_commit() {
        let blame = GitBlameResult {
            root: "/r".into(),
            path: "f".into(),
            rev: None,
            lines: 3,
            hunks: vec![
                BlameHunk {
                    line: 1,
                    count: 2,
                    commit: "a".repeat(40),
                },
                BlameHunk {
                    line: 3,
                    count: 1,
                    commit: GIT_UNCOMMITTED.into(),
                },
            ],
            commits: vec![
                commit(&"a".repeat(40), "Ada"),
                commit(GIT_UNCOMMITTED, "Not Committed Yet"),
            ],
            truncated: false,
        };
        let lines = blame_lines(&blame, "one\ntwo\nthree\n");
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("aaaaaaa Ada"), "{}", lines[0]);
        assert!(lines[0].ends_with("   1) one"), "{}", lines[0]);
        assert!(lines[1].ends_with("   2) two"));
        assert!(
            lines[2].starts_with("0000000 Not Committed Yet"),
            "{}",
            lines[2]
        );
    }
}
