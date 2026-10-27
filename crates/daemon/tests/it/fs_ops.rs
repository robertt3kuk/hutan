//! File operations (protocol 0.54, capability `fs.ops`): a real daemon on a temporary
//! `HUTAN_HOME`, real client connections, and a trash that is a folder (`FolderTrash`), so that
//! nothing here touches the user's Trash. Confinement (`..`, links, `.git`, roots), never
//! overwriting, case-only renames, cross-root refusal, batch partial failure, copying, trash
//! tickets, `fs.changed` with `origin: self`, and what an agent has to ask for.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hutan_client::{Client, ClientError};
use hutan_daemon::files::confine::FsError;
use hutan_daemon::files::trash::{FolderTrash, Trash};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ApprovalList, ApprovalListRequest};
use hutan_protocol::control::ErrorBody;
use hutan_protocol::fs::{
    COPY_ENTRIES_MAX, ChangeKind, FsCopy, FsCopyItem, FsCreate, FsCreateKind, FsKind, FsMove,
    FsMoveItem, FsOpItem, FsOpResults, FsOrigin, FsRename, FsRestore, FsTrash, FsWatch,
    FsWatchPath, OPS_ITEMS_MAX,
};
use hutan_protocol::messages::{AgentState, Request, WorkspaceOpen, WorkspaceOpened};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

use crate::common::WAIT;
use crate::files::{
    Env, answer, change_of, code_of, connect_from, next_approval, reason, refused, report_working,
    shell_in,
};

/// A daemon whose trash is a folder next to its workspace.
struct Ops {
    env: Env,
    /// Where the trash puts things.
    trash: PathBuf,
    _home: TempDir,
}

impl Ops {
    async fn new() -> Self {
        Self::with("", |_| {}).await
    }

    async fn with(toml: &str, tweak: impl FnOnce(&mut hutan_daemon::DaemonConfig)) -> Self {
        let home = tempfile::tempdir().unwrap();
        let trash = home.path().canonicalize().unwrap().join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        let backend: Arc<dyn Trash> = Arc::new(FolderTrash::new(&trash));
        let env = Env::with(toml, |config| {
            config.fs_trash = Some(backend);
            tweak(config);
        })
        .await;
        assert!(env.client.supports_fs_ops());
        Self {
            env,
            trash,
            _home: home,
        }
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.env.at(relative)
    }

    fn client(&self) -> &Client {
        &self.env.client
    }

    async fn stop(self) {
        self.env.stop().await;
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn error_of(item: &FsOpItem) -> &ErrorBody {
    item.error
        .as_ref()
        .unwrap_or_else(|| panic!("{} succeeded: {item:?}", item.path.display()))
}

fn reason_of(body: &ErrorBody) -> String {
    body.data
        .as_ref()
        .and_then(|data| data["reason"].as_str())
        .unwrap_or("")
        .to_owned()
}

fn create(path: &Path, kind: FsCreateKind, parents: bool) -> FsCreate {
    FsCreate {
        path: path.to_owned(),
        kind,
        parents,
    }
}

fn rename(from: &Path, to: &Path) -> FsRename {
    FsRename {
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

fn mv(from: &Path, to: &Path) -> FsMoveItem {
    FsMoveItem {
        from: from.to_owned(),
        to: to.to_owned(),
    }
}

fn cp(from: &Path, to: &Path) -> FsCopyItem {
    FsCopyItem {
        from: from.to_owned(),
        to: to.to_owned(),
        picked_from: false,
    }
}

fn trash_of(paths: &[&Path]) -> FsTrash {
    FsTrash {
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
    }
}

// ---- create -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn create_makes_files_and_folders_and_never_replaces_anything() {
    let ops = Ops::new().await;
    let c = ops.client();

    let file = c
        .fs_create(create(&ops.at("a.txt"), FsCreateKind::File, false))
        .await
        .unwrap();
    assert_eq!(
        (file.kind, file.size, &file.path, file.name.as_str()),
        (FsKind::File, 0, &ops.at("a.txt"), "a.txt")
    );
    let dir = c
        .fs_create(create(&ops.at("d"), FsCreateKind::Dir, false))
        .await
        .unwrap();
    assert_eq!(dir.kind, FsKind::Dir);
    assert!(ops.at("d").is_dir());

    // Something there already: `conflict` / `exists`, and what was there stays.
    std::fs::write(ops.at("full.txt"), "keep me").unwrap();
    let error = refused(
        c.fs_create(create(&ops.at("full.txt"), FsCreateKind::File, false))
            .await,
        ErrorCode::Conflict,
    );
    assert_eq!(reason(&error), "exists");
    assert_eq!(
        std::fs::read_to_string(ops.at("full.txt")).unwrap(),
        "keep me"
    );
    let error = refused(
        c.fs_create(create(&ops.at("d"), FsCreateKind::Dir, false))
            .await,
        ErrorCode::Conflict,
    );
    assert_eq!(reason(&error), "exists");
    // Not even through a link that points at a file outside the roots.
    std::fs::write(ops.env.outside.join("target.txt"), "outside").unwrap();
    std::os::unix::fs::symlink(ops.env.outside.join("target.txt"), ops.at("link")).unwrap();
    let error = refused(
        c.fs_create(create(&ops.at("link"), FsCreateKind::File, false))
            .await,
        ErrorCode::Conflict,
    );
    assert_eq!(reason(&error), "exists");
    assert_eq!(
        std::fs::read_to_string(ops.env.outside.join("target.txt")).unwrap(),
        "outside"
    );

    // A folder that is missing is an error, unless `parents` asks for it.
    refused(
        c.fs_create(create(&ops.at("x/y/new.txt"), FsCreateKind::File, false))
            .await,
        ErrorCode::NotFound,
    );
    assert!(!ops.at("x").exists());
    let deep = c
        .fs_create(create(&ops.at("x/y/z"), FsCreateKind::Dir, true))
        .await
        .unwrap();
    assert_eq!(deep.path, ops.at("x/y/z"));
    assert!(ops.at("x/y/z").is_dir());
    let nested = c
        .fs_create(create(&ops.at("p/q/file.rs"), FsCreateKind::File, true))
        .await
        .unwrap();
    assert_eq!(nested.kind, FsKind::File);
    assert!(ops.at("p/q/file.rs").is_file());

    // Names that are not names.
    refused(
        c.fs_create(create(Path::new("relative.txt"), FsCreateKind::File, false))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        c.fs_create(create(&ops.at("d/.."), FsCreateKind::Dir, false))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        c.fs_create(create(&ops.at(&"n".repeat(300)), FsCreateKind::File, false))
            .await,
        ErrorCode::BadRequest,
    );
    ops.stop().await;
}

// ---- confinement ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn nothing_reaches_outside_the_roots_through_a_path_a_dot_dot_or_a_link() {
    let ops = Ops::new().await;
    let c = ops.client();
    let outside = ops.env.outside.clone();
    std::fs::write(outside.join("secret.txt"), "outside").unwrap();
    std::fs::write(ops.at("inside.txt"), "inside").unwrap();
    std::fs::create_dir_all(ops.at("sub")).unwrap();
    std::os::unix::fs::symlink(&outside, ops.at("leakdir")).unwrap();

    // A plain path outside, and `..` out of the root.
    let up = ops.at("sub/../../outside/new.txt");
    for path in [outside.join("new.txt"), up.clone()] {
        let error = refused(
            c.fs_create(create(&path, FsCreateKind::File, false)).await,
            ErrorCode::Forbidden,
        );
        assert_eq!(reason(&error), "outside_roots", "{}", path.display());
    }
    let error = refused(
        c.fs_rename(rename(&ops.at("inside.txt"), &outside.join("moved.txt")))
            .await,
        ErrorCode::Forbidden,
    );
    assert_eq!(reason(&error), "outside_roots");
    let error = refused(
        c.fs_rename(rename(&outside.join("secret.txt"), &ops.at("taken.txt")))
            .await,
        ErrorCode::Forbidden,
    );
    assert_eq!(reason(&error), "outside_roots");
    let trashed = c
        .fs_trash(trash_of(&[&outside.join("secret.txt")]))
        .await
        .unwrap();
    assert_eq!(reason_of(error_of(&trashed.items[0])), "outside_roots");
    assert_eq!(trashed.ticket, None);
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "outside"
    );
    assert!(!outside.join("new.txt").exists() && !outside.join("moved.txt").exists());

    // Batches report each item: outside the roots is that item's error.
    let moved = c
        .fs_move(FsMove {
            items: vec![
                mv(&ops.at("inside.txt"), &outside.join("moved.txt")),
                mv(&ops.at("inside.txt"), &ops.at("sub/inside.txt")),
            ],
        })
        .await
        .unwrap();
    assert_eq!(reason_of(error_of(&moved.items[0])), "outside_roots");
    assert!(moved.items[1].entry.is_some());
    let copied = c
        .fs_copy(FsCopy {
            items: vec![
                cp(&ops.at("sub/inside.txt"), &outside.join("copied.txt")),
                cp(&outside.join("secret.txt"), &ops.at("secret.txt")),
            ],
        })
        .await
        .unwrap();
    assert_eq!(reason_of(error_of(&copied.items[0])), "outside_roots");
    assert_eq!(reason_of(error_of(&copied.items[1])), "outside_roots");
    assert!(!ops.at("secret.txt").exists() && !outside.join("copied.txt").exists());

    // A link that leaves the roots: its folder is outside, whatever the name says.
    let error = refused(
        c.fs_create(create(
            &ops.at("leakdir/new.txt"),
            FsCreateKind::File,
            false,
        ))
        .await,
        ErrorCode::Forbidden,
    );
    assert_eq!(reason(&error), "symlink");
    let error = refused(
        c.fs_rename(rename(&ops.at("leakdir/secret.txt"), &ops.at("stolen.txt")))
            .await,
        ErrorCode::Forbidden,
    );
    assert_eq!(reason(&error), "symlink");
    let trashed = c
        .fs_trash(trash_of(&[&ops.at("leakdir/secret.txt")]))
        .await
        .unwrap();
    assert_eq!(reason_of(error_of(&trashed.items[0])), "symlink");
    assert_eq!(trashed.ticket, None);
    assert!(outside.join("secret.txt").exists() && !outside.join("new.txt").exists());

    // The link itself is an entry of the root: it is renamed, copied and trashed as a link, and
    // what it points at is not touched.
    let renamed = c
        .fs_rename(rename(&ops.at("leakdir"), &ops.at("leaklink")))
        .await
        .unwrap();
    assert_eq!(renamed.kind, FsKind::Symlink);
    let copied = c
        .fs_copy(FsCopy {
            items: vec![cp(&ops.at("leaklink"), &ops.at("leaklink2"))],
        })
        .await
        .unwrap();
    assert_eq!(
        copied.items[0].entry.as_ref().unwrap().kind,
        FsKind::Symlink
    );
    assert_eq!(std::fs::read_link(ops.at("leaklink2")).unwrap(), outside);
    let trashed = c
        .fs_trash(trash_of(&[&ops.at("leaklink"), &ops.at("leaklink2")]))
        .await
        .unwrap();
    assert!(trashed.items.iter().all(|item| item.entry.is_some()));
    assert!(!ops.at("leaklink").exists() && ops.at("leaklink").symlink_metadata().is_err());
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "outside"
    );
    ops.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_below_dot_git_and_never_a_root_itself() {
    let ops = Ops::new().await;
    let c = ops.client();
    std::fs::create_dir_all(ops.at(".git/objects")).unwrap();
    std::fs::write(ops.at(".git/config"), "[core]").unwrap();
    std::fs::create_dir_all(ops.at("sub/.git")).unwrap();
    std::fs::write(ops.at("a.txt"), "a").unwrap();

    for path in [ops.at(".git/new"), ops.at("sub/.git/new"), ops.at(".git")] {
        let error = refused(
            c.fs_create(create(&path, FsCreateKind::File, false)).await,
            ErrorCode::Forbidden,
        );
        assert_eq!(reason(&error), "git_dir", "{}", path.display());
    }
    // `parents` does not make a `.git` either.
    let error = refused(
        c.fs_create(create(&ops.at("fresh/.git/x"), FsCreateKind::File, true))
            .await,
        ErrorCode::Forbidden,
    );
    assert_eq!(reason(&error), "git_dir");
    assert!(!ops.at("fresh").exists());

    // Into `.git`, out of it, and the folder itself.
    for (from, to) in [
        (ops.at("a.txt"), ops.at(".git/a.txt")),
        (ops.at(".git/config"), ops.at("config")),
        (ops.at("sub/.git"), ops.at("sub/dotgit")),
    ] {
        let error = refused(c.fs_rename(rename(&from, &to)).await, ErrorCode::Forbidden);
        assert_eq!(reason(&error), "git_dir", "{}", from.display());
    }
    let batch = c
        .fs_move(FsMove {
            items: vec![mv(&ops.at("a.txt"), &ops.at(".git/a.txt"))],
        })
        .await
        .unwrap();
    assert_eq!(reason_of(error_of(&batch.items[0])), "git_dir");
    let copied = c
        .fs_copy(FsCopy {
            items: vec![
                cp(&ops.at("a.txt"), &ops.at(".git/a.txt")),
                cp(&ops.at(".git/config"), &ops.at("config")),
                cp(&ops.at(".git"), &ops.at("dotgit")),
            ],
        })
        .await
        .unwrap();
    assert!(
        copied
            .items
            .iter()
            .all(|item| reason_of(error_of(item)) == "git_dir")
    );
    let trashed = c
        .fs_trash(trash_of(&[&ops.at(".git/config"), &ops.at(".git")]))
        .await
        .unwrap();
    assert!(
        trashed
            .items
            .iter()
            .all(|item| reason_of(error_of(item)) == "git_dir")
    );
    assert_eq!(trashed.ticket, None);
    assert!(ops.at(".git/config").is_file() && ops.at("a.txt").is_file());
    assert_eq!(names(&ops.at(".git")), ["config", "objects"]);

    // A root itself.
    let error = refused(
        c.fs_rename(rename(&ops.env.root, &ops.env.outside.join("elsewhere")))
            .await,
        ErrorCode::Forbidden,
    );
    assert!(reason(&error) == "root" || reason(&error) == "outside_roots");
    let trashed = c.fs_trash(trash_of(&[&ops.env.root])).await.unwrap();
    assert_eq!(reason_of(error_of(&trashed.items[0])), "root");
    assert!(ops.env.root.is_dir());
    ops.stop().await;
}

// ---- rename and move --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn rename_never_overwrites_and_a_case_only_rename_works() {
    let ops = Ops::new().await;
    let c = ops.client();
    std::fs::write(ops.at("old.txt"), "old").unwrap();
    std::fs::write(ops.at("taken.txt"), "taken").unwrap();
    std::fs::create_dir_all(ops.at("dir/inner")).unwrap();
    std::fs::write(ops.at("Readme.md"), "# readme").unwrap();

    let entry = c
        .fs_rename(rename(&ops.at("old.txt"), &ops.at("new.txt")))
        .await
        .unwrap();
    assert_eq!((entry.path, entry.size), (ops.at("new.txt"), 3));
    assert!(!ops.at("old.txt").exists());

    let error = refused(
        c.fs_rename(rename(&ops.at("new.txt"), &ops.at("taken.txt")))
            .await,
        ErrorCode::Conflict,
    );
    assert_eq!(reason(&error), "exists");
    assert_eq!(
        std::fs::read_to_string(ops.at("taken.txt")).unwrap(),
        "taken"
    );
    assert_eq!(
        std::fs::read_to_string(ops.at("new.txt")).unwrap(),
        "old",
        "the source stays when the target is taken"
    );

    // Only the case of the name changes (APFS sees the target as taken by the file itself).
    let entry = c
        .fs_rename(rename(&ops.at("Readme.md"), &ops.at("README.md")))
        .await
        .unwrap();
    assert_eq!(entry.name, "README.md");
    assert!(names(&ops.env.root).contains(&"README.md".to_owned()));
    assert!(!names(&ops.env.root).contains(&"Readme.md".to_owned()));
    assert_eq!(
        std::fs::read_to_string(ops.at("README.md")).unwrap(),
        "# readme"
    );
    // A folder, case-only too.
    c.fs_rename(rename(&ops.at("dir"), &ops.at("Dir")))
        .await
        .unwrap();
    assert!(names(&ops.env.root).contains(&"Dir".to_owned()));
    assert!(ops.at("Dir/inner").is_dir());

    // To itself, into itself, a missing source.
    refused(
        c.fs_rename(rename(&ops.at("new.txt"), &ops.at("new.txt")))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        c.fs_rename(rename(&ops.at("Dir"), &ops.at("Dir/inner/Dir")))
            .await,
        ErrorCode::BadRequest,
    );
    refused(
        c.fs_rename(rename(&ops.at("ghost"), &ops.at("spirit")))
            .await,
        ErrorCode::NotFound,
    );
    // The folder of the target must exist.
    refused(
        c.fs_rename(rename(&ops.at("new.txt"), &ops.at("no/such/new.txt")))
            .await,
        ErrorCode::NotFound,
    );
    // A folder moves with everything in it.
    std::fs::write(ops.at("Dir/inner/f.txt"), "f").unwrap();
    c.fs_rename(rename(&ops.at("Dir/inner"), &ops.at("moved")))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(ops.at("moved/f.txt")).unwrap(), "f");
    ops.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_and_move_stay_in_one_root_copy_does_not() {
    let ops = Ops::new().await;
    let c = ops.client();
    // A second root: another workspace.
    let other = ops.env.outside.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other = other.canonicalize().unwrap();
    let _: WorkspaceOpened = c
        .call(Request::WorkspaceOpen(WorkspaceOpen {
            path: other.clone(),
            created_by: None,
        }))
        .await
        .unwrap();
    std::fs::write(ops.at("a.txt"), "a").unwrap();

    let error = refused(
        c.fs_rename(rename(&ops.at("a.txt"), &other.join("a.txt")))
            .await,
        ErrorCode::Unsupported,
    );
    assert_eq!(reason(&error), "cross_root");
    let moved = c
        .fs_move(FsMove {
            items: vec![mv(&ops.at("a.txt"), &other.join("a.txt"))],
        })
        .await
        .unwrap();
    let body = error_of(&moved.items[0]);
    assert_eq!(
        (body.code, reason_of(body)),
        (ErrorCode::Unsupported, "cross_root".to_owned())
    );
    assert!(ops.at("a.txt").exists() && !other.join("a.txt").exists());

    // Copy, then trash: how a file changes roots.
    let copied = c
        .fs_copy(FsCopy {
            items: vec![cp(&ops.at("a.txt"), &other.join("a.txt"))],
        })
        .await
        .unwrap();
    assert_eq!(
        copied.items[0].entry.as_ref().unwrap().path,
        other.join("a.txt")
    );
    assert_eq!(std::fs::read_to_string(other.join("a.txt")).unwrap(), "a");
    let trashed = c.fs_trash(trash_of(&[&ops.at("a.txt")])).await.unwrap();
    assert!(trashed.ticket.is_some() && !ops.at("a.txt").exists());
    ops.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_reports_every_item_and_one_failure_does_not_undo_the_rest() {
    let ops = Ops::new().await;
    let c = ops.client();
    for name in ["one.txt", "two.txt", "three.txt", "four.txt"] {
        std::fs::write(ops.at(name), name).unwrap();
    }
    std::fs::create_dir_all(ops.at("dest")).unwrap();
    std::fs::write(ops.at("dest/two.txt"), "in the way").unwrap();

    let moved = c
        .fs_move(FsMove {
            items: vec![
                mv(&ops.at("one.txt"), &ops.at("dest/one.txt")),
                mv(&ops.at("two.txt"), &ops.at("dest/two.txt")),
                mv(&ops.at("missing.txt"), &ops.at("dest/missing.txt")),
                mv(&ops.at("three.txt"), &ops.at("dest/three.txt")),
            ],
        })
        .await
        .unwrap();
    assert_eq!(moved.items.len(), 4);
    assert_eq!(
        moved
            .items
            .iter()
            .map(|item| item.path.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>(),
        ["one.txt", "two.txt", "missing.txt", "three.txt"],
        "in the order of the request"
    );
    assert_eq!(
        moved.items[0].entry.as_ref().unwrap().path,
        ops.at("dest/one.txt")
    );
    let body = error_of(&moved.items[1]);
    assert_eq!(
        (body.code, reason_of(body)),
        (ErrorCode::Conflict, "exists".to_owned())
    );
    assert_eq!(error_of(&moved.items[2]).code, ErrorCode::NotFound);
    assert!(moved.items[3].entry.is_some());
    assert_eq!(names(&ops.at("dest")), ["one.txt", "three.txt", "two.txt"]);
    assert_eq!(
        std::fs::read_to_string(ops.at("dest/two.txt")).unwrap(),
        "in the way"
    );
    assert!(ops.at("two.txt").exists() && ops.at("four.txt").exists());

    // A batch has a size limit; an empty one is fine.
    let too_many = FsMove {
        items: (0..=OPS_ITEMS_MAX)
            .map(|i| mv(&ops.at(&format!("a{i}")), &ops.at(&format!("b{i}"))))
            .collect(),
    };
    refused(c.fs_move(too_many).await, ErrorCode::BadRequest);
    let none: FsOpResults = c.fs_move(FsMove { items: vec![] }).await.unwrap();
    assert!(none.items.is_empty());
    ops.stop().await;
}

// ---- copy -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn copy_makes_an_independent_copy_of_a_file_a_tree_and_a_link() {
    let ops = Ops::new().await;
    let c = ops.client();
    std::fs::create_dir_all(ops.at("src/deep/er")).unwrap();
    std::fs::write(ops.at("src/a.txt"), "a").unwrap();
    std::fs::write(ops.at("src/deep/b.txt"), "b").unwrap();
    std::fs::write(ops.at("src/deep/er/c.txt"), "c").unwrap();
    std::os::unix::fs::symlink("a.txt", ops.at("src/rel")).unwrap();
    std::os::unix::fs::symlink(&ops.env.outside, ops.at("src/out")).unwrap();
    std::fs::write(ops.at("single.txt"), "single").unwrap();

    let copied = c
        .fs_copy(FsCopy {
            items: vec![
                cp(&ops.at("src"), &ops.at("src copy")),
                cp(&ops.at("single.txt"), &ops.at("single 2.txt")),
                cp(&ops.at("src"), &ops.at("src copy")),
                cp(&ops.at("src"), &ops.at("src/deep/inside")),
                cp(&ops.at("missing"), &ops.at("missing 2")),
            ],
        })
        .await
        .unwrap();
    assert_eq!(copied.items[0].entry.as_ref().unwrap().kind, FsKind::Dir);
    assert_eq!(copied.items[1].entry.as_ref().unwrap().size, 6);
    assert_eq!(reason_of(error_of(&copied.items[2])), "exists");
    assert_eq!(error_of(&copied.items[3]).code, ErrorCode::BadRequest);
    assert_eq!(error_of(&copied.items[4]).code, ErrorCode::NotFound);
    assert!(!ops.at("src/deep/inside").exists());

    assert_eq!(
        std::fs::read_to_string(ops.at("src copy/deep/er/c.txt")).unwrap(),
        "c"
    );
    assert_eq!(
        std::fs::read_link(ops.at("src copy/rel")).unwrap(),
        PathBuf::from("a.txt"),
        "a link is copied as a link"
    );
    assert_eq!(
        std::fs::read_link(ops.at("src copy/out")).unwrap(),
        ops.env.outside
    );
    // Independent: a change of the copy does not reach the original.
    std::fs::write(ops.at("src copy/a.txt"), "changed").unwrap();
    assert_eq!(std::fs::read_to_string(ops.at("src/a.txt")).unwrap(), "a");
    assert_eq!(
        std::fs::read_to_string(ops.at("single.txt")).unwrap(),
        "single"
    );
    ops.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_is_bounded_in_entries_and_bytes_and_leaves_nothing_half_made() {
    let ops = Ops::new().await;
    let c = ops.client();
    // More entries than a copy takes.
    std::fs::create_dir_all(ops.at("many")).unwrap();
    for i in 0..=COPY_ENTRIES_MAX {
        std::fs::File::create(ops.at(&format!("many/f{i}"))).unwrap();
    }
    // More bytes: a sparse file costs nothing.
    std::fs::create_dir_all(ops.at("big")).unwrap();
    std::fs::File::create(ops.at("big/sparse"))
        .unwrap()
        .set_len(3 << 30)
        .unwrap();
    let copied = c
        .fs_copy(FsCopy {
            items: vec![
                cp(&ops.at("many"), &ops.at("many 2")),
                cp(&ops.at("big"), &ops.at("big 2")),
                cp(&ops.at("big/sparse"), &ops.at("sparse 2")),
            ],
        })
        .await
        .unwrap();
    for item in &copied.items {
        let body = error_of(item);
        assert_eq!(
            (body.code, reason_of(body)),
            (ErrorCode::Unsupported, "too_large".to_owned()),
            "{}",
            item.path.display()
        );
    }
    assert!(
        !ops.at("many 2").exists() && !ops.at("big 2").exists() && !ops.at("sparse 2").exists()
    );
    ops.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picked_source_may_lie_outside_the_roots_for_the_user_and_never_for_an_agent() {
    let ops = Ops::with("[control]\nagents = \"allow\"\n", |_| {}).await;
    let c = ops.client();
    let picked = ops.env.outside.join("logo.png");
    std::fs::write(&picked, "png").unwrap();
    std::fs::create_dir_all(ops.env.outside.join("folder")).unwrap();
    std::fs::write(ops.env.outside.join("folder/in.txt"), "in").unwrap();

    let item = |from: &Path, to: &Path| FsCopyItem {
        picked_from: true,
        ..cp(from, to)
    };
    // Without `picked_from` the source is outside the roots.
    let plain = c
        .fs_copy(FsCopy {
            items: vec![cp(&picked, &ops.at("logo.png"))],
        })
        .await
        .unwrap();
    assert_eq!(reason_of(error_of(&plain.items[0])), "outside_roots");
    let done = c
        .fs_copy(FsCopy {
            items: vec![
                item(&picked, &ops.at("logo.png")),
                item(&ops.env.outside.join("folder"), &ops.at("folder")),
                // The target is always confined.
                item(&picked, &ops.env.outside.join("copy.png")),
            ],
        })
        .await
        .unwrap();
    assert_eq!(done.items[0].entry.as_ref().unwrap().size, 3);
    assert_eq!(
        std::fs::read_to_string(ops.at("folder/in.txt")).unwrap(),
        "in"
    );
    assert_eq!(reason_of(error_of(&done.items[2])), "outside_roots");
    assert!(!ops.env.outside.join("copy.png").exists());

    // An agent's `picked_from` counts for nothing.
    let session = shell_in(&ops.env).await;
    report_working(&ops.env.t, session.id, AgentState::Working, 1).await;
    let agent = connect_from(&ops.env.t, session.id).await;
    let done = agent
        .fs_copy(FsCopy {
            items: vec![item(&picked, &ops.at("agent.png"))],
        })
        .await
        .unwrap();
    assert_eq!(reason_of(error_of(&done.items[0])), "outside_roots");
    assert!(!ops.at("agent.png").exists());
    ops.stop().await;
}

// ---- trash and restore ------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn trash_then_restore_puts_everything_back_through_its_ticket() {
    let ops = Ops::new().await;
    let c = ops.client();
    std::fs::create_dir_all(ops.at("proj/sub")).unwrap();
    std::fs::write(ops.at("proj/sub/f.txt"), "f").unwrap();
    std::fs::write(ops.at("note.txt"), "note").unwrap();
    std::os::unix::fs::symlink("note.txt", ops.at("alias")).unwrap();

    let trashed = c
        .fs_trash(trash_of(&[
            &ops.at("proj"),
            &ops.at("note.txt"),
            &ops.at("ghost"),
            &ops.at("alias"),
        ]))
        .await
        .unwrap();
    let ticket = trashed.ticket.clone().expect("a ticket");
    assert_eq!(trashed.items.len(), 4);
    // What was trashed is described as it was; what failed says why.
    assert_eq!(trashed.items[0].entry.as_ref().unwrap().kind, FsKind::Dir);
    assert_eq!(trashed.items[1].entry.as_ref().unwrap().size, 4);
    assert_eq!(error_of(&trashed.items[2]).code, ErrorCode::NotFound);
    assert_eq!(
        trashed.items[3].entry.as_ref().unwrap().kind,
        FsKind::Symlink
    );
    assert!(!ops.at("proj").exists() && !ops.at("note.txt").exists());
    assert!(ops.at("alias").symlink_metadata().is_err());
    // Gone from the folder, in the trash, not deleted.
    assert_eq!(std::fs::read_dir(&ops.trash).unwrap().count(), 3);

    let restored = c
        .fs_restore(FsRestore {
            ticket: ticket.clone(),
        })
        .await
        .unwrap();
    assert_eq!(restored.items.len(), 3);
    assert!(restored.items.iter().all(|item| item.entry.is_some()));
    assert_eq!(
        std::fs::read_to_string(ops.at("proj/sub/f.txt")).unwrap(),
        "f"
    );
    assert_eq!(std::fs::read_to_string(ops.at("note.txt")).unwrap(), "note");
    assert_eq!(
        std::fs::read_link(ops.at("alias")).unwrap(),
        PathBuf::from("note.txt")
    );
    // A ticket is used once.
    let error = refused(
        c.fs_restore(FsRestore { ticket }).await,
        ErrorCode::NotFound,
    );
    assert_eq!(reason(&error), "ticket");
    refused(
        c.fs_restore(FsRestore {
            ticket: "01K7F3B5N2Q9W8E4R6T1Y0V3TK".into(),
        })
        .await,
        ErrorCode::NotFound,
    );
    // Nothing trashed, no ticket.
    let none = c.fs_trash(trash_of(&[&ops.at("ghost")])).await.unwrap();
    assert_eq!(none.ticket, None);
    ops.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_never_overwrites_and_what_could_not_come_back_stays_on_the_ticket() {
    let ops = Ops::new().await;
    let c = ops.client();
    std::fs::write(ops.at("a.txt"), "a").unwrap();
    std::fs::write(ops.at("b.txt"), "b").unwrap();
    std::fs::write(ops.at("c.txt"), "c").unwrap();
    std::fs::create_dir_all(ops.at("gone")).unwrap();
    std::fs::write(ops.at("gone/d.txt"), "d").unwrap();
    let trashed = c
        .fs_trash(trash_of(&[
            &ops.at("a.txt"),
            &ops.at("b.txt"),
            &ops.at("c.txt"),
            &ops.at("gone/d.txt"),
        ]))
        .await
        .unwrap();
    let ticket = trashed.ticket.unwrap();
    // A new file took a place; the Trash was emptied of one item; a folder is gone.
    std::fs::write(ops.at("a.txt"), "new a").unwrap();
    let in_trash: Vec<PathBuf> = std::fs::read_dir(&ops.trash)
        .unwrap()
        .map(|slot| slot.unwrap().path())
        .collect();
    for slot in &in_trash {
        if slot.join("b.txt").exists() {
            std::fs::remove_file(slot.join("b.txt")).unwrap();
        }
    }
    std::fs::remove_dir_all(ops.at("gone")).unwrap();

    let restored = c
        .fs_restore(FsRestore {
            ticket: ticket.clone(),
        })
        .await
        .unwrap();
    assert_eq!(reason_of(error_of(&restored.items[0])), "exists");
    assert_eq!(error_of(&restored.items[1]).code, ErrorCode::NotFound);
    assert!(restored.items[2].entry.is_some());
    assert_eq!(error_of(&restored.items[3]).code, ErrorCode::NotFound);
    assert_eq!(std::fs::read_to_string(ops.at("a.txt")).unwrap(), "new a");
    assert_eq!(std::fs::read_to_string(ops.at("c.txt")).unwrap(), "c");

    // What came back left the ticket; the rest can be tried again once the way is clear.
    std::fs::remove_file(ops.at("a.txt")).unwrap();
    let again = c.fs_restore(FsRestore { ticket }).await.unwrap();
    assert_eq!(again.items.len(), 3, "c.txt is not on the ticket any more");
    assert!(again.items[0].entry.is_some());
    assert_eq!(std::fs::read_to_string(ops.at("a.txt")).unwrap(), "a");
    ops.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tickets_are_bounded_and_expire() {
    let ops = Ops::with("", |config| config.fs_limits.tickets_max = 2).await;
    let c = ops.client();
    let mut tickets = Vec::new();
    for name in ["one", "two", "three"] {
        std::fs::write(ops.at(name), name).unwrap();
        tickets.push(
            c.fs_trash(trash_of(&[&ops.at(name)]))
                .await
                .unwrap()
                .ticket
                .unwrap(),
        );
    }
    // The oldest ticket was forgotten; the files are still in the Trash.
    refused(
        c.fs_restore(FsRestore {
            ticket: tickets[0].clone(),
        })
        .await,
        ErrorCode::NotFound,
    );
    c.fs_restore(FsRestore {
        ticket: tickets[2].clone(),
    })
    .await
    .unwrap();
    assert!(ops.at("three").exists() && !ops.at("two").exists());
    ops.stop().await;

    // A ticket that is older than its lifetime is gone.
    let ops = Ops::with("", |config| config.fs_limits.ticket_ttl = Duration::ZERO).await;
    std::fs::write(ops.at("x"), "x").unwrap();
    let ticket = ops
        .client()
        .fs_trash(trash_of(&[&ops.at("x")]))
        .await
        .unwrap()
        .ticket
        .unwrap();
    let error = refused(
        ops.client().fs_restore(FsRestore { ticket }).await,
        ErrorCode::NotFound,
    );
    assert!(error.to_string().contains("expired"), "{error}");
    assert!(!ops.at("x").exists());
    ops.stop().await;
}

/// A volume without a Trash.
#[derive(Debug)]
struct NoTrash;

impl Trash for NoTrash {
    fn trash(&self, path: &Path) -> Result<PathBuf, FsError> {
        Err(FsError::unsupported(
            "no_trash",
            format!("{} has no Trash on its volume", path.display()),
        ))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_trash_nothing_is_deleted() {
    let ops = Ops::with("", |config| config.fs_trash = Some(Arc::new(NoTrash))).await;
    std::fs::write(ops.at("keep.txt"), "keep").unwrap();
    let trashed = ops
        .client()
        .fs_trash(trash_of(&[&ops.at("keep.txt")]))
        .await
        .unwrap();
    let body = error_of(&trashed.items[0]);
    assert_eq!(
        (body.code, reason_of(body)),
        (ErrorCode::Unsupported, "no_trash".to_owned())
    );
    assert_eq!(trashed.ticket, None);
    assert_eq!(std::fs::read_to_string(ops.at("keep.txt")).unwrap(), "keep");
    ops.stop().await;
}

// ---- events -----------------------------------------------------------------------------------

async fn watch_root(client: &Client, root: &Path) {
    client
        .fs_watch(FsWatch {
            paths: vec![FsWatchPath {
                path: root.to_owned(),
                recursive: true,
            }],
            picked: false,
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_changes_an_operation_causes_are_its_connections_own_and_nobody_elses() {
    let ops = Ops::new().await;
    let mut events = ops.env.client.events();
    let bystander = ops.env.t.connect().await;
    let mut seen = bystander.events();
    watch_root(&ops.env.client, &ops.env.root).await;
    watch_root(&bystander, &ops.env.root).await;
    let c = ops.env.client.clone();

    let is_own =
        |change: &hutan_protocol::fs::FsChange| matches!(change.origin, Some(FsOrigin::Own { .. }));

    // create
    c.fs_create(create(&ops.at("made.txt"), FsCreateKind::File, false))
        .await
        .unwrap();
    let created = change_of(&mut events, &ops.at("made.txt"), |_| true).await;
    assert!(is_own(&created), "{created:?}");
    let other = change_of(&mut seen, &ops.at("made.txt"), |_| true).await;
    assert_eq!(other.origin, Some(FsOrigin::External));

    // rename: both names
    c.fs_rename(rename(&ops.at("made.txt"), &ops.at("renamed.txt")))
        .await
        .unwrap();
    let renamed = change_of(&mut events, &ops.at("renamed.txt"), |_| true).await;
    assert!(is_own(&renamed), "{renamed:?}");
    let _ = change_of(&mut seen, &ops.at("renamed.txt"), |_| true).await;

    // copy of a folder, and a file made inside it by the clone
    std::fs::create_dir_all(ops.at("tree/inner")).unwrap();
    std::fs::write(ops.at("tree/inner/f.txt"), "f").unwrap();
    c.fs_copy(FsCopy {
        items: vec![cp(&ops.at("tree"), &ops.at("tree 2"))],
    })
    .await
    .unwrap();
    let copied = change_of(&mut events, &ops.at("tree 2"), |_| true).await;
    assert!(is_own(&copied), "{copied:?}");

    // move
    c.fs_move(FsMove {
        items: vec![mv(&ops.at("renamed.txt"), &ops.at("tree/renamed.txt"))],
    })
    .await
    .unwrap();
    let moved = change_of(&mut events, &ops.at("tree/renamed.txt"), |_| true).await;
    assert!(is_own(&moved), "{moved:?}");

    // trash: removed, and restore: created again
    let ticket = c
        .fs_trash(trash_of(&[&ops.at("tree 2")]))
        .await
        .unwrap()
        .ticket
        .unwrap();
    let removed = change_of(&mut events, &ops.at("tree 2"), |change| {
        change.kind == ChangeKind::Removed
    })
    .await;
    assert!(is_own(&removed), "{removed:?}");
    let removed_seen = change_of(&mut seen, &ops.at("tree 2"), |change| {
        change.kind == ChangeKind::Removed
    })
    .await;
    assert_eq!(removed_seen.origin, Some(FsOrigin::External));
    c.fs_restore(FsRestore { ticket }).await.unwrap();
    let back = change_of(&mut events, &ops.at("tree 2"), |change| {
        change.kind != ChangeKind::Removed
    })
    .await;
    assert!(is_own(&back), "{back:?}");

    // A change by somebody else in the same folder is still not ours.
    std::fs::write(ops.at("outsider.txt"), "x").unwrap();
    let theirs = change_of(&mut events, &ops.at("outsider.txt"), |_| true).await;
    assert_eq!(theirs.origin, Some(FsOrigin::External));
    ops.stop().await;
}

// ---- an agent asks ----------------------------------------------------------------------------

async fn pending(user: &Client) -> usize {
    let list: ApprovalList = user
        .call(Request::ApprovalList(ApprovalListRequest::default()))
        .await
        .unwrap();
    list.approvals.len()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_file_operations_ask_under_ask_and_the_users_do_not() {
    let ops = Ops::with(
        "[control]\nagents = \"ask\"\napproval_timeout_secs = 20\n",
        |_| {},
    )
    .await;
    std::fs::write(ops.at("a.txt"), "a").unwrap();
    let session = shell_in(&ops.env).await;
    report_working(&ops.env.t, session.id, AgentState::Working, 1).await;
    let agent = connect_from(&ops.env.t, session.id).await;

    // Reading is free; create waits for the answer, and nothing happens before it.
    assert!(agent.fs_read_all(&ops.at("a.txt"), false).await.is_ok());
    assert_eq!(pending(&ops.env.client).await, 0);
    let creating = {
        let agent = agent.clone();
        let path = ops.at("made.txt");
        tokio::spawn(async move {
            agent
                .fs_create(create(&path, FsCreateKind::File, false))
                .await
        })
    };
    let approval = next_approval(&ops.env.client).await;
    assert!(
        approval.title.contains("create the file") && approval.title.contains("made.txt"),
        "{}",
        approval.title
    );
    assert!(!ops.at("made.txt").exists(), "nothing before the answer");
    answer(&ops.env.client, approval.id, "allow_once").await;
    timeout(WAIT, creating).await.unwrap().unwrap().unwrap();
    assert!(ops.at("made.txt").exists());

    // Denied: `forbidden`, nothing changes.
    let moving = {
        let agent = agent.clone();
        let (from, to) = (ops.at("a.txt"), ops.at("b.txt"));
        tokio::spawn(async move { agent.fs_rename(rename(&from, &to)).await })
    };
    let approval = next_approval(&ops.env.client).await;
    assert!(
        approval.title.contains("rename") && approval.title.contains("b.txt"),
        "{}",
        approval.title
    );
    answer(&ops.env.client, approval.id, "deny").await;
    refused(
        timeout(WAIT, moving).await.unwrap().unwrap(),
        ErrorCode::Forbidden,
    );
    assert!(ops.at("a.txt").exists() && !ops.at("b.txt").exists());

    // Trash, copy, move, restore ask too; each describes itself.
    let trashing = {
        let agent = agent.clone();
        let path = ops.at("a.txt");
        tokio::spawn(async move { agent.fs_trash(trash_of(&[&path])).await })
    };
    let approval = next_approval(&ops.env.client).await;
    assert!(
        approval.title.contains("to the Trash") && approval.title.contains("a.txt"),
        "{}",
        approval.title
    );
    answer(&ops.env.client, approval.id, "allow_once").await;
    let trashed = timeout(WAIT, trashing).await.unwrap().unwrap().unwrap();
    assert!(!ops.at("a.txt").exists());
    let restoring = {
        let agent = agent.clone();
        let ticket = trashed.ticket.clone().unwrap();
        tokio::spawn(async move { agent.fs_restore(FsRestore { ticket }).await })
    };
    let approval = next_approval(&ops.env.client).await;
    assert!(
        approval.title.contains("Trash ticket"),
        "{}",
        approval.title
    );
    answer(&ops.env.client, approval.id, "allow_once").await;
    timeout(WAIT, restoring).await.unwrap().unwrap().unwrap();
    assert!(ops.at("a.txt").exists());
    let copying = {
        let agent = agent.clone();
        let (from, to) = (ops.at("a.txt"), ops.at("a2.txt"));
        tokio::spawn(async move {
            agent
                .fs_copy(FsCopy {
                    items: vec![cp(&from, &to)],
                })
                .await
        })
    };
    let approval = next_approval(&ops.env.client).await;
    assert!(approval.title.contains("copy"), "{}", approval.title);
    answer(&ops.env.client, approval.id, "allow_once").await;
    timeout(WAIT, copying).await.unwrap().unwrap().unwrap();
    let batch = {
        let agent = agent.clone();
        let (from, to) = (ops.at("a2.txt"), ops.at("a3.txt"));
        tokio::spawn(async move {
            agent
                .fs_move(FsMove {
                    items: vec![mv(&from, &to)],
                })
                .await
        })
    };
    let approval = next_approval(&ops.env.client).await;
    assert!(approval.title.contains("move"), "{}", approval.title);
    answer(&ops.env.client, approval.id, "allow_once").await;
    timeout(WAIT, batch).await.unwrap().unwrap().unwrap();
    assert!(ops.at("a3.txt").exists() && !ops.at("a2.txt").exists());

    // The user at a plain shell is never asked.
    ops.env
        .client
        .fs_rename(rename(&ops.at("a3.txt"), &ops.at("a4.txt")))
        .await
        .unwrap();
    ops.env
        .client
        .fs_trash(trash_of(&[&ops.at("a4.txt")]))
        .await
        .unwrap();
    sleep(Duration::from_millis(50)).await;
    assert_eq!(pending(&ops.env.client).await, 0);
    ops.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn under_read_only_an_agent_cannot_operate_on_files_and_under_allow_it_is_not_asked() {
    for (policy, allowed) in [("read_only", false), ("allow", true)] {
        let ops = Ops::with(&format!("[control]\nagents = \"{policy}\"\n"), |_| {}).await;
        std::fs::write(ops.at("a.txt"), "a").unwrap();
        let session = shell_in(&ops.env).await;
        report_working(&ops.env.t, session.id, AgentState::Working, 1).await;
        let agent = connect_from(&ops.env.t, session.id).await;

        let results = [
            agent
                .fs_create(create(&ops.at("made"), FsCreateKind::Dir, false))
                .await
                .map(drop),
            agent
                .fs_rename(rename(&ops.at("a.txt"), &ops.at("b.txt")))
                .await
                .map(drop),
            agent
                .fs_move(FsMove {
                    items: vec![mv(&ops.at("b.txt"), &ops.at("made/b.txt"))],
                })
                .await
                .map(drop),
            agent
                .fs_copy(FsCopy {
                    items: vec![cp(&ops.at("made/b.txt"), &ops.at("c.txt"))],
                })
                .await
                .map(drop),
            agent
                .fs_trash(trash_of(&[&ops.at("c.txt")]))
                .await
                .map(drop),
            agent
                .fs_restore(FsRestore {
                    ticket: "01K7F3B5N2Q9W8E4R6T1Y0V3TK".into(),
                })
                .await
                .map(drop),
        ];
        if allowed {
            // Everything ran, in order, and none was asked; the restore fails on its ticket.
            assert!(results[..5].iter().all(Result::is_ok), "{results:?}");
            assert_eq!(
                code_of(results[5].as_ref().unwrap_err()),
                ErrorCode::NotFound
            );
            assert!(ops.at("made/b.txt").exists() && !ops.at("c.txt").exists());
            assert_eq!(pending(&ops.env.client).await, 0);
        } else {
            for result in &results {
                let error = refused(result.as_ref().map_err(clone_error), ErrorCode::Forbidden);
                assert!(error.to_string().contains("read-only"), "{error}");
            }
            assert!(ops.at("a.txt").exists());
            assert!(!ops.at("made").exists() && !ops.at("b.txt").exists());
        }
        // Looking is allowed under every policy.
        assert!(
            agent
                .fs_read_all(&ops.at("made/b.txt"), false)
                .await
                .is_ok()
                == allowed
        );
        ops.stop().await;
    }
}

fn clone_error(error: &ClientError) -> ClientError {
    match error {
        ClientError::Remote(body) => ClientError::Remote(body.clone()),
        other => panic!("not a remote error: {other:?}"),
    }
}
