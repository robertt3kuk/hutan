//! `<home>/scratch/<session>`: the images a client pasted into a session's terminal
//! (`docs/design.md` § Terminal drops and pastes). The desktop writes them; the daemon owns the
//! sessions, so it removes a session's folder when the session is closed (or its workspace is
//! deleted) and, at start, the folders of sessions it no longer has.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;

use hutan_core::SessionId;
use tracing::warn;

/// Removes `<scratch>/<session>` with everything in it. A folder that is not there is fine.
pub fn remove(scratch: &Path, session: SessionId) {
    let dir = scratch.join(session.to_string());
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => warn!(dir = %dir.display(), "could not remove pasted images: {error}"),
    }
}

/// Removes the folders of `scratch` whose name is not the id of a session in `keep` (leftovers
/// of sessions closed while no daemon ran, or of a daemon that stopped mid-removal). Files
/// directly in `scratch` are left alone.
pub fn sweep(scratch: &Path, keep: &HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(scratch) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_dir && !keep.contains(entry.file_name().to_string_lossy().as_ref()) {
            let path = entry.path();
            if let Err(error) = std::fs::remove_dir_all(&path) {
                warn!(dir = %path.display(), "could not remove pasted images: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_folder_is_removed_and_a_missing_one_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionId::new();
        remove(dir.path(), session);
        let folder = dir.path().join(session.to_string());
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("paste-1.png"), b"x").unwrap();
        remove(dir.path(), session);
        assert!(!folder.exists());
    }

    #[test]
    fn the_sweep_keeps_the_sessions_it_is_told_and_plain_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["live", "gone"] {
            std::fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        std::fs::write(dir.path().join("note.txt"), b"x").unwrap();
        sweep(dir.path(), &HashSet::from(["live".to_owned()]));
        assert!(dir.path().join("live").exists());
        assert!(!dir.path().join("gone").exists());
        assert!(dir.path().join("note.txt").exists());
        // No folder at all is fine.
        sweep(&dir.path().join("none"), &HashSet::new());
    }
}
