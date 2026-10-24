//! Filesystem rules of projects and workspaces: the Home folder, a project's identity (the git
//! common directory of the deepest repository around a folder, else the folder), and which
//! project — or Home — a folder belongs to (protocol 0.19, ADR 0014). Blocking functions;
//! callers run them off the async threads (or accept a few metadata calls).

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use hutan_protocol::control::{ErrorBody, ErrorCode};

use crate::placement;

/// Creates the Home folder (mode `0700`, with missing parents) unless it exists, and returns
/// its canonical path. An existing folder is never touched and nothing is created inside it.
pub fn ensure_home_folder(folder: &Path) -> io::Result<PathBuf> {
    match fs::metadata(folder) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a directory", folder.display()),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(folder)?;
        }
        Err(e) => return Err(e),
    }
    fs::canonicalize(folder)
}

/// What identifies a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    /// The git common directory (canonical) of the deepest repository around the folder: the
    /// main checkout and every linked worktree of a repository share it, a submodule and a
    /// nested repository have their own. `None` outside git.
    pub common_dir: Option<PathBuf>,
    /// The project's folder: the repository's main checkout, else the folder itself.
    pub root: PathBuf,
}

impl Ident {
    /// Display name of the project: the root's folder name.
    pub fn name(&self) -> String {
        folder_name(&self.root)
    }
}

fn folder_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| root.display().to_string())
}

/// The project of the deepest git repository containing `dir` (canonical); `None` outside git.
/// A `.git` entry that cannot be read (a worktree whose repository is gone) makes the
/// checkout's folder a plain folder project.
pub fn identify(dir: &Path) -> Option<Ident> {
    let checkout = git_root(dir)?;
    Some(match placement::repository_of(&checkout) {
        Some(repository) => Ident {
            common_dir: Some(repository.common_dir),
            root: repository.main_root.unwrap_or(checkout),
        },
        None => Ident {
            common_dir: None,
            root: checkout,
        },
    })
}

/// What decides between Home and a project for a folder.
#[derive(Debug, Clone)]
pub struct HomeRules {
    /// The user's home directory (canonical): neither it nor a repository rooted in it is a
    /// project, so a dotfiles repository does not swallow every folder below `~`.
    pub user_home: Option<PathBuf>,
    /// The Home workspace's folder (canonical): it and what lies below it, outside any
    /// repository, belong to Home.
    pub home_folder: PathBuf,
}

/// Where a session that starts in a folder goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assignment {
    Home,
    Project(Ident),
}

impl HomeRules {
    /// The rules for these folders; ones that cannot be resolved are used as they are.
    pub fn new(user_home: Option<PathBuf>, home_folder: PathBuf) -> Self {
        let resolve = |path: PathBuf| fs::canonicalize(&path).unwrap_or(path);
        Self {
            user_home: user_home.map(resolve),
            home_folder: resolve(home_folder),
        }
    }

    /// A repository rooted here is never a project: `/`, the home directory (a dotfiles
    /// repository) and the Home folder itself (after *Track changes*, `project.git_init`):
    /// each would swallow every folder below it.
    fn is_catch_all(&self, root: &Path) -> bool {
        root == Path::new("/")
            || self.user_home.as_deref() == Some(root)
            || root == self.home_folder
    }

    /// The assignment of `dir` (canonical): the deepest enclosing git repository's project;
    /// else Home for `/`, the home directory and the Home folder (and what lies below it);
    /// else the folder itself as a project.
    pub fn assign(&self, dir: &Path) -> Assignment {
        if self.is_catch_all(dir) || dir == self.home_folder {
            return Assignment::Home;
        }
        if let Some(ident) = identify(dir)
            && !self.is_catch_all(&ident.root)
        {
            return Assignment::Project(ident);
        }
        if dir.starts_with(&self.home_folder) {
            return Assignment::Home;
        }
        Assignment::Project(Ident {
            common_dir: None,
            root: dir.to_path_buf(),
        })
    }

    /// [`assign`](Self::assign) for a path as a client gave it: a directory that does not
    /// exist (any more) goes to Home.
    pub fn assign_path(&self, dir: Option<&Path>) -> Assignment {
        dir.and_then(|dir| fs::canonicalize(dir).ok())
            .filter(|dir| dir.is_dir())
            .map_or(Assignment::Home, |dir| self.assign(&dir))
    }
}

/// A folder as `workspace.open` and `workspace.add_root` see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// The requested folder, canonical (symlinks resolved).
    pub path: PathBuf,
    /// The project's folder: the main checkout of the repository containing `path`, else `path`
    /// itself.
    pub root: PathBuf,
    /// The repository's common git directory, when `path` is in a repository.
    pub common_dir: Option<PathBuf>,
}

impl Folder {
    /// Display name of the project: the root's folder name.
    pub fn name(&self) -> String {
        folder_name(&self.root)
    }

    pub fn ident(&self) -> Ident {
        Ident {
            common_dir: self.common_dir.clone(),
            root: self.root.clone(),
        }
    }
}

/// Canonicalises `path` (absolute, existing directory) and finds its repository root.
pub fn resolve_folder(path: &Path) -> Result<Folder, ErrorBody> {
    if !path.is_absolute() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("`{}` is not an absolute path", path.display()),
        ));
    }
    let canonical = match fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("{} does not exist", path.display()),
            ));
        }
        Err(e) => {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("cannot use {}: {e}", path.display()),
            ));
        }
    };
    if !canonical.is_dir() {
        return Err(ErrorBody::new(
            ErrorCode::BadRequest,
            format!("{} is not a directory", path.display()),
        ));
    }
    let ident = identify(&canonical).unwrap_or_else(|| Ident {
        common_dir: None,
        root: canonical.clone(),
    });
    Ok(Folder {
        path: canonical,
        root: ident.root,
        common_dir: ident.common_dir,
    })
}

/// The nearest ancestor of `start` (or `start` itself) holding a `.git` entry: a directory for a
/// repository, a file for a worktree or submodule.
pub fn git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
        .map(Path::to_path_buf)
}

/// `path` (canonical) is `root` or lies below it. A stored root may reach the same place
/// through a symlink, so its canonical form counts too.
pub fn contains(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
        || fs::canonicalize(root).is_ok_and(|canonical| path.starts_with(canonical))
}

/// Both paths name the same place, given as written or after resolving symlinks.
pub fn same_place(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn canonical(dir: &tempfile::TempDir) -> PathBuf {
        fs::canonicalize(dir.path()).unwrap()
    }

    #[test]
    fn the_home_folder_is_created_private_once_and_left_alone_after() {
        let dir = tempfile::tempdir().unwrap();
        let home = canonical(&dir).join("deep/Hutan");
        let made = ensure_home_folder(&home).unwrap();
        assert_eq!(made, home);
        let mode = fs::metadata(&home).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        // An existing folder keeps its mode and gets no new files.
        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_home_folder(&home).unwrap();
        assert_eq!(
            fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);

        let file = canonical(&dir).join("file");
        fs::write(&file, b"x").unwrap();
        assert!(ensure_home_folder(&file).is_err());
    }

    #[test]
    fn a_folder_inside_a_repository_resolves_to_the_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let repo = base.join("repo");
        let deep = repo.join("src/inner");
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();

        let folder = resolve_folder(&deep).unwrap();
        assert_eq!(folder.path, deep);
        assert_eq!(folder.root, repo);
        assert_eq!(folder.name(), "repo");

        // A `.git` file marks a root as well, even when the git directory it names is gone.
        let worktree = base.join("wt");
        fs::create_dir(&worktree).unwrap();
        fs::write(worktree.join(".git"), "gitdir: /elsewhere").unwrap();
        let folder = resolve_folder(&worktree).unwrap();
        assert_eq!((folder.root, folder.common_dir), (worktree, None));

        // Outside any repository the folder is its own root.
        let plain = base.join("plain");
        fs::create_dir(&plain).unwrap();
        let folder = resolve_folder(&plain).unwrap();
        assert_eq!(folder.name(), "plain");
        assert_eq!(folder.root, plain);
    }

    #[test]
    fn symlinks_resolve_and_bad_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let real = base.join("real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, base.join("link")).unwrap();
        assert_eq!(resolve_folder(&base.join("link")).unwrap().path, real);

        assert_eq!(
            resolve_folder(Path::new("relative")).unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            resolve_folder(&base.join("missing")).unwrap_err().code,
            ErrorCode::NotFound
        );
        let file = base.join("file");
        fs::write(&file, b"x").unwrap();
        assert_eq!(
            resolve_folder(&file).unwrap_err().code,
            ErrorCode::BadRequest
        );
    }

    #[test]
    fn containment_is_by_path_component_and_follows_symlinked_roots() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let real = base.join("real");
        fs::create_dir_all(real.join("sub")).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(contains(&real, &real));
        assert!(contains(&real, &real.join("sub")));
        assert!(contains(&link, &real.join("sub")), "root given by symlink");
        assert!(!contains(&base.join("rea"), &real), "not a string prefix");
        assert!(!contains(&real.join("sub"), &real));
    }

    #[test]
    fn same_place_follows_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let real = base.join("real");
        fs::create_dir_all(real.join("sub")).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(same_place(&link, &real));
        assert!(!same_place(&link, &real.join("sub")));
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "protocol.file.allow=always"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo(base: &Path, name: &str) -> PathBuf {
        let root = base.join(name);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "first"]);
        root
    }

    fn ident(common_dir: &Path, root: &Path) -> Ident {
        Ident {
            common_dir: Some(fs::canonicalize(common_dir).unwrap()),
            root: root.to_path_buf(),
        }
    }

    #[test]
    fn identity_is_the_deepest_repositorys_common_directory() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let main = repo(&base, "app");
        fs::create_dir_all(main.join("src/inner")).unwrap();
        let own = ident(&main.join(".git"), &main);

        // The main checkout, from any folder in it.
        assert_eq!(identify(&main), Some(own.clone()));
        assert_eq!(identify(&main.join("src/inner")), Some(own.clone()));

        // A linked worktree is the same project, rooted at the main checkout.
        let wt = base.join("app-wt");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                wt.to_str().unwrap(),
            ],
        );
        fs::create_dir(wt.join("nested")).unwrap();
        assert_eq!(identify(&wt), Some(own.clone()));
        assert_eq!(identify(&wt.join("nested")), Some(own.clone()));

        // A repository inside another one's working tree is its own project.
        let nested = repo(&main.join("vendor"), "inner");
        assert_eq!(
            identify(&nested),
            Some(ident(&nested.join(".git"), &nested))
        );
        assert_eq!(identify(&main.join("vendor")), Some(own.clone()));

        // So is a submodule: its common directory is its module directory in the parent's.
        let upstream = repo(&base, "lib");
        git(
            &main,
            &[
                "submodule",
                "add",
                "-q",
                upstream.to_str().unwrap(),
                "libs/lib",
            ],
        );
        let sub = main.join("libs/lib");
        let found = identify(&sub.join(".")).unwrap();
        assert_eq!(found.root, sub);
        assert_eq!(
            found.common_dir,
            Some(fs::canonicalize(main.join(".git/modules/libs/lib")).unwrap())
        );
        assert_ne!(Some(own), Some(found));

        // A folder outside git has no repository.
        let plain = base.join("plain");
        fs::create_dir(&plain).unwrap();
        assert_eq!(identify(&plain), None);
    }

    #[test]
    fn a_worktree_whose_repository_is_gone_is_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let main = repo(&base, "app");
        let wt = base.join("app-wt");
        git(
            &main,
            &["worktree", "add", "-q", "-b", "f", wt.to_str().unwrap()],
        );
        fs::remove_dir_all(&main).unwrap();
        assert_eq!(
            identify(&wt),
            Some(Ident {
                common_dir: None,
                root: wt
            })
        );
    }

    #[test]
    fn home_the_home_folder_and_the_root_are_not_projects() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let user_home = base.join("me");
        let home_folder = user_home.join("Hutan");
        fs::create_dir_all(home_folder.join("scratch")).unwrap();
        let docs = user_home.join("docs");
        fs::create_dir(&docs).unwrap();
        let rules = HomeRules::new(Some(user_home.clone()), home_folder.clone());
        let project = |root: &Path| {
            Assignment::Project(Ident {
                common_dir: None,
                root: root.to_path_buf(),
            })
        };

        assert_eq!(rules.assign(Path::new("/")), Assignment::Home);
        assert_eq!(rules.assign(&user_home), Assignment::Home);
        assert_eq!(rules.assign(&home_folder), Assignment::Home);
        assert_eq!(rules.assign(&home_folder.join("scratch")), Assignment::Home);
        // Other folders outside git are projects of their own.
        assert_eq!(rules.assign(&docs), project(&docs));

        // A repository inside the Home folder is a project; one that *is* the home directory
        // (a dotfiles repository) is not, and does not swallow the folders below it.
        let app = repo(&home_folder, "app");
        assert!(matches!(rules.assign(&app), Assignment::Project(i) if i.root == app));
        git(&user_home, &["init", "-q", "-b", "main"]);
        assert_eq!(rules.assign(&user_home), Assignment::Home);
        assert_eq!(rules.assign(&docs), project(&docs));
        assert_eq!(rules.assign(&home_folder.join("scratch")), Assignment::Home);

        // No folder, or one that is gone, goes to Home.
        assert_eq!(rules.assign_path(None), Assignment::Home);
        assert_eq!(
            rules.assign_path(Some(&base.join("gone"))),
            Assignment::Home
        );
        assert_eq!(rules.assign_path(Some(&docs)), project(&docs));
    }

    #[test]
    fn a_home_folder_under_version_control_stays_home() {
        let dir = tempfile::tempdir().unwrap();
        let base = canonical(&dir);
        let user_home = base.join("me");
        let home_folder = user_home.join("Hutan");
        fs::create_dir_all(home_folder.join("scratch")).unwrap();
        let rules = HomeRules::new(Some(user_home), home_folder.clone());
        git(&home_folder, &["init", "-q", "-b", "main"]);

        // Tracked by git, the Home folder and what is below it are still Home …
        assert_eq!(rules.assign(&home_folder), Assignment::Home);
        assert_eq!(rules.assign(&home_folder.join("scratch")), Assignment::Home);
        // … while a repository of its own inside it is still a project.
        let app = repo(&home_folder, "app");
        assert!(matches!(rules.assign(&app), Assignment::Project(i) if i.root == app));
    }
}
