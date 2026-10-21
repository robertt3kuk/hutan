//! The hidden refs of a repository (`refs/hutan/checkpoints/*`): listed with their commit
//! trailers (what reconcile rebuilds rows from), deleted in one `update-ref --stdin`
//! transaction, packed with `pack-refs --include` when many are loose. Commands run in the
//! repository's common git directory, so the refs of a removed worktree are still reachable.
//! Hutan never runs `gc` or `prune`: the objects of a deleted ref age out under git's own
//! settings (ADR 0019 Decision 7).

use std::fs;
use std::path::Path;

use super::git::{self, Git, os_args};
use super::{CheckpointError, Checkpoints, REF_PREFIX};

/// Loose checkpoint refs past which they are packed (spike §3.7: more than 200).
pub const PACK_LOOSE_REFS: usize = 200;

/// One checkpoint ref and what its commit says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefInfo {
    /// The full ref name.
    pub name: String,
    pub commit: String,
    pub tree: String,
    /// The first parent (an end's start commit).
    pub parent: Option<String>,
    /// The commit's time.
    pub created_ms: u64,
    /// The `Hutan-Session`, `Hutan-Turn`, `Hutan-Kind` and `Hutan-Checkpoint` trailers.
    pub session: Option<String>,
    pub turn: Option<String>,
    pub kind: Option<String>,
    pub checkpoint: Option<String>,
}

/// What changed between two commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub files: u64,
    /// Lines; binary files count none.
    pub added: u64,
    pub removed: u64,
}

const FORMAT: &str = "--format=%(refname)%00%(objectname)%00%(tree)%00%(parent)%00\
%(creatordate:unix)%00%(trailers:key=Hutan-Session,valueonly,separator=%x2C)%00\
%(trailers:key=Hutan-Turn,valueonly,separator=%x2C)%00\
%(trailers:key=Hutan-Kind,valueonly,separator=%x2C)%00\
%(trailers:key=Hutan-Checkpoint,valueonly,separator=%x2C)";

impl Checkpoints {
    fn repo_git(&self, common_dir: &Path) -> Git {
        Git::new(
            common_dir,
            &self.inner.config.git_env,
            self.inner.config.read_timeout,
        )
    }

    /// Every checkpoint ref of the repository at `common_dir`.
    pub async fn list_refs(&self, common_dir: &Path) -> Result<Vec<RefInfo>, CheckpointError> {
        let output = self
            .repo_git(common_dir)
            .read_ok(&["for-each-ref", FORMAT, REF_PREFIX])
            .await?;
        Ok(parse_refs(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Deletes `refs` (missing ones are no error) in one transaction.
    pub async fn delete_refs(
        &self,
        common_dir: &Path,
        refs: &[String],
    ) -> Result<(), CheckpointError> {
        if refs.is_empty() {
            return Ok(());
        }
        let mut input = String::new();
        for name in refs {
            if !name.starts_with(REF_PREFIX) || name.contains(char::is_whitespace) {
                return Err(CheckpointError::InvalidLabel(format!(
                    "`{name}` is not a checkpoint ref"
                )));
            }
            input.push_str("delete ");
            input.push_str(name);
            input.push('\n');
        }
        self.repo_git(common_dir)
            .write_ok(
                &os_args(&["update-ref", "--no-deref", "--stdin"]),
                Some(input.into_bytes()),
            )
            .await
            .map(drop)
    }

    /// Packs the checkpoint refs (and no tag or branch) when more than [`PACK_LOOSE_REFS`] are
    /// loose; `true` when it did.
    pub async fn pack_refs_if_many(&self, common_dir: &Path) -> Result<bool, CheckpointError> {
        let loose = count_files(&common_dir.join(REF_PREFIX.trim_end_matches('/')));
        if loose <= PACK_LOOSE_REFS {
            return Ok(false);
        }
        self.repo_git(common_dir)
            .write_ok(&os_args(&["pack-refs", "--include", "refs/hutan/*"]), None)
            .await?;
        tracing::info!(repository = %common_dir.display(), loose, "packed the checkpoint refs");
        Ok(true)
    }

    /// Files, lines added and lines removed between the commits `from` and `to`.
    pub async fn diff_stat(
        &self,
        common_dir: &Path,
        from: &str,
        to: &str,
    ) -> Result<DiffStat, CheckpointError> {
        let output = self
            .repo_git(common_dir)
            .read_ok(&[
                "diff-tree",
                "-r",
                "-z",
                "--numstat",
                "--no-renames",
                from,
                to,
            ])
            .await?;
        Ok(parse_numstat(&output.stdout))
    }
}

fn parse_refs(text: &str) -> Vec<RefInfo> {
    let field = |value: &str| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    };
    text.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\0').collect();
            let [
                name,
                commit,
                tree,
                parents,
                date,
                session,
                turn,
                kind,
                checkpoint,
            ] = parts.as_slice()
            else {
                return None;
            };
            Some(RefInfo {
                name: (*name).to_owned(),
                commit: (*commit).to_owned(),
                tree: (*tree).to_owned(),
                parent: parents.split_whitespace().next().map(str::to_owned),
                created_ms: date.trim().parse::<u64>().unwrap_or(0).saturating_mul(1000),
                session: field(session),
                turn: field(turn),
                kind: field(kind),
                checkpoint: field(checkpoint),
            })
        })
        .collect()
}

/// `diff-tree --numstat -z`: `<added>\t<removed>\t<path>\0`, `-` for a binary file.
fn parse_numstat(bytes: &[u8]) -> DiffStat {
    let mut stat = DiffStat::default();
    for record in git::nul_records(bytes) {
        let text = String::from_utf8_lossy(record);
        let mut fields = text.splitn(3, '\t');
        let (Some(added), Some(removed), Some(_path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        stat.files += 1;
        stat.added += added.parse::<u64>().unwrap_or(0);
        stat.removed += removed.parse::<u64>().unwrap_or(0);
    }
    stat
}

/// Regular files under `dir`, recursively (loose refs; lock files count too, they are rare).
fn count_files(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => count_files(&entry.path()),
            Ok(kind) if kind.is_file() => 1,
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_parse_with_their_trailers() {
        let text = "refs/hutan/checkpoints/S/T\0c1\0t1\0p1 p2\x001700000000\0S\0T\0end\0CP\n\
                    refs/hutan/checkpoints/S/CP-start\0c2\0t2\0\x001700000001\0S\0\0start\0CP\n\
                    garbage\n";
        let refs = parse_refs(text);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].parent.as_deref(), Some("p1"));
        assert_eq!(refs[0].created_ms, 1_700_000_000_000);
        assert_eq!(refs[0].turn.as_deref(), Some("T"));
        assert_eq!(refs[0].checkpoint.as_deref(), Some("CP"));
        assert_eq!(refs[1].parent, None);
        assert_eq!(refs[1].turn, None);
        assert_eq!(refs[1].kind.as_deref(), Some("start"));
    }

    #[test]
    fn numstat_counts_files_and_lines_and_binaries_as_files() {
        let stat = parse_numstat(b"3\t1\ta.txt\x00-\t-\tlogo.png\x000\t2\tdir/b.txt\x00");
        assert_eq!(
            stat,
            DiffStat {
                files: 3,
                added: 3,
                removed: 3
            }
        );
    }
}
