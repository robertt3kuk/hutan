//! Where a language server's workspace root is: the **topmost** folder with one of its root
//! markers above the file, within the project, with a per-project override.
//!
//! Evidence (`docs/spikes/2026-10-22-editor-research/lsp.md` §5): the nearest marker splits a
//! monorepo into one server per package (nvim-lspconfig #3818, #4457, #4227); the topmost one
//! makes the server see the whole workspace (a Cargo workspace, a `tsconfig` with references).
//! The walk never leaves the project: the folder of the workspace the file belongs to, or the
//! git checkout that contains it, whichever is deeper. A file outside both gets the nearest
//! marker only, since nothing says where its project ends.

use std::path::{Path, PathBuf};

/// How many folders up a file without a known project is searched.
const UNBOUNDED_WALK: usize = 32;

/// The innermost of the project folder and the git checkout around `file`: where the search for
/// markers stops. `None` when the file is in neither.
pub fn boundary(file: &Path, project_root: Option<&Path>) -> Option<PathBuf> {
    let git = file
        .ancestors()
        .skip(1)
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf);
    let project = project_root
        .filter(|root| file.starts_with(root))
        .map(Path::to_path_buf);
    match (git, project) {
        (Some(git), Some(project)) => Some(if git.starts_with(&project) {
            git
        } else {
            project
        }),
        (git, project) => git.or(project),
    }
}

/// The root of the server with `markers` for `file` (canonical, absolute): the topmost folder
/// from the file's own up to `boundary` that has a marker; without any, `boundary`, else the
/// file's folder. Without a `boundary` the nearest marker wins.
pub fn detect(file: &Path, markers: &[String], boundary: Option<&Path>) -> PathBuf {
    let Some(start) = file.parent() else {
        return file.to_path_buf();
    };
    let has_marker = |dir: &Path| markers.iter().any(|marker| dir.join(marker).exists());
    match boundary.filter(|b| start.starts_with(b)) {
        Some(boundary) => start
            .ancestors()
            .take_while(|dir| dir.starts_with(boundary))
            .filter(|dir| has_marker(dir))
            .last()
            .map_or_else(|| boundary.to_path_buf(), Path::to_path_buf),
        None => start
            .ancestors()
            .take(UNBOUNDED_WALK)
            .find(|dir| has_marker(dir))
            .unwrap_or(start)
            .to_path_buf(),
    }
}

/// The root a project's `[lsp.projects.<p>.roots]` names for `file`, when it contains the file:
/// `configured` is absolute or relative to the project's folder.
pub fn overridden(file: &Path, configured: &str, project_root: Option<&Path>) -> Option<PathBuf> {
    let configured = Path::new(configured);
    let root = if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        project_root?.join(configured)
    };
    // Not canonical when it does not exist; the file must be below it either way.
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    file.starts_with(&root).then_some(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    fn touch(dir: &Path, relative: &str) -> PathBuf {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        path
    }

    fn canonical_tmp() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        (tmp, dir)
    }

    #[test]
    fn the_topmost_marker_within_the_project_wins_over_the_nearest() {
        let (_tmp, dir) = canonical_tmp();
        touch(&dir, "mono/Cargo.toml");
        touch(&dir, "mono/crates/a/Cargo.toml");
        let file = touch(&dir, "mono/crates/a/src/lib.rs");
        let m = markers(&["Cargo.toml"]);
        let root = detect(&file, &m, Some(&dir));
        assert_eq!(root, dir.join("mono"));
        // The marker above the project is out of reach.
        let root = detect(&file, &m, Some(&dir.join("mono/crates")));
        assert_eq!(root, dir.join("mono/crates/a"));
    }

    #[test]
    fn without_a_marker_the_project_is_the_root_else_the_files_folder() {
        let (_tmp, dir) = canonical_tmp();
        let file = touch(&dir, "p/docs/guide.md");
        assert_eq!(
            detect(&file, &markers(&[]), Some(&dir.join("p"))),
            dir.join("p")
        );
        assert_eq!(
            detect(&file, &markers(&["x.toml"]), None),
            dir.join("p/docs")
        );
    }

    #[test]
    fn without_a_known_project_the_nearest_marker_is_the_root() {
        let (_tmp, dir) = canonical_tmp();
        touch(&dir, "Cargo.toml");
        touch(&dir, "inner/Cargo.toml");
        let file = touch(&dir, "inner/src/lib.rs");
        assert_eq!(
            detect(&file, &markers(&["Cargo.toml"]), None),
            dir.join("inner")
        );
    }

    #[test]
    fn the_boundary_is_the_deeper_of_the_project_and_the_git_checkout() {
        let (_tmp, dir) = canonical_tmp();
        let file = touch(&dir, "ws/repo/src/a.rs");
        std::fs::create_dir_all(dir.join("ws/repo/.git")).unwrap();
        // A workspace folder holding several checkouts: each checkout is its own project.
        assert_eq!(
            boundary(&file, Some(&dir.join("ws"))),
            Some(dir.join("ws/repo"))
        );
        // A project folder inside a checkout bounds the walk itself.
        assert_eq!(
            boundary(&file, Some(&dir.join("ws/repo/src"))),
            Some(dir.join("ws/repo/src"))
        );
        // A project that does not contain the file says nothing.
        assert_eq!(
            boundary(&file, Some(&dir.join("elsewhere"))),
            Some(dir.join("ws/repo"))
        );
        let loose = touch(&dir, "loose/b.rs");
        assert_eq!(boundary(&loose, None), None);
    }

    #[test]
    fn a_project_override_must_contain_the_file() {
        let (_tmp, dir) = canonical_tmp();
        let file = touch(&dir, "p/crates/core/src/lib.rs");
        assert_eq!(
            overridden(&file, "crates/core", Some(&dir.join("p"))),
            Some(dir.join("p/crates/core"))
        );
        assert_eq!(
            overridden(&file, &dir.join("p/crates").display().to_string(), None),
            Some(dir.join("p/crates"))
        );
        assert_eq!(
            overridden(&file, "crates/other", Some(&dir.join("p"))),
            None
        );
        assert_eq!(overridden(&file, "crates/core", None), None);
    }
}
