//! What the user decided about folders: whether a language server may start in one.
//!
//! A server runs the project's build scripts and procedural macros, executes paths named in
//! workspace settings, and (rust-analyzer) does not sandbox any of it. So a folder is trusted
//! before a server starts in it, once per folder, for everything below it. The decision is the
//! user's alone (`lsp.trust` is `Access::UserOnly`), persisted, and revocable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hutan_protocol::lsp::{LspTrust, LspTrustEntry};

use crate::config::LspTrustMode;

/// How a folder stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Trusted,
    /// The user said no (or `[lsp] trust = "never"`): nothing is asked.
    Denied,
    /// Nobody decided: the request to trust is pending.
    Ask,
}

/// The decisions, by folder.
#[derive(Debug, Clone, Default)]
pub struct TrustBook {
    decisions: BTreeMap<PathBuf, LspTrustEntry>,
}

impl TrustBook {
    pub fn new(entries: impl IntoIterator<Item = LspTrustEntry>) -> Self {
        Self {
            decisions: entries
                .into_iter()
                .map(|entry| (entry.folder.clone(), entry))
                .collect(),
        }
    }

    /// The decision that covers `root`: the one of the deepest folder that contains it.
    pub fn decision_for(&self, root: &Path) -> Option<&LspTrustEntry> {
        root.ancestors().find_map(|dir| self.decisions.get(dir))
    }

    /// Whether a server may start in `root`: the user's decision, else `mode`.
    pub fn standing(&self, root: &Path, mode: LspTrustMode) -> Standing {
        match self.decision_for(root).map(|entry| entry.decision) {
            Some(LspTrust::Trusted) => Standing::Trusted,
            Some(LspTrust::Denied) => Standing::Denied,
            Some(LspTrust::Ask) | None => match mode {
                LspTrustMode::Always => Standing::Trusted,
                LspTrustMode::Never => Standing::Denied,
                LspTrustMode::Ask => Standing::Ask,
            },
        }
    }

    /// Records a decision (`Trusted` or `Denied`); `Ask` forgets the folder's own.
    pub fn set(&mut self, entry: LspTrustEntry) {
        match entry.decision {
            LspTrust::Ask => {
                self.decisions.remove(&entry.folder);
            }
            _ => {
                self.decisions.insert(entry.folder.clone(), entry);
            }
        }
    }

    /// The decisions, folders sorted.
    pub fn entries(&self) -> Vec<LspTrustEntry> {
        self.decisions.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(folder: &str, decision: LspTrust) -> LspTrustEntry {
        LspTrustEntry {
            folder: folder.into(),
            decision,
            decided_at_ms: 1,
            decided_by: "cli".into(),
        }
    }

    #[test]
    fn the_deepest_decision_covers_a_root_and_the_mode_decides_the_rest() {
        let book = TrustBook::new([
            entry("/work", LspTrust::Trusted),
            entry("/work/untrusted-dep", LspTrust::Denied),
        ]);
        let ask = LspTrustMode::Ask;
        assert_eq!(
            book.standing(Path::new("/work/repo"), ask),
            Standing::Trusted
        );
        assert_eq!(book.standing(Path::new("/work"), ask), Standing::Trusted);
        assert_eq!(
            book.standing(Path::new("/work/untrusted-dep/crates/a"), ask),
            Standing::Denied
        );
        assert_eq!(book.standing(Path::new("/other"), ask), Standing::Ask);
        assert_eq!(
            book.standing(Path::new("/other"), LspTrustMode::Always),
            Standing::Trusted
        );
        assert_eq!(
            book.standing(Path::new("/other"), LspTrustMode::Never),
            Standing::Denied
        );
        // A decision of the user beats the default either way.
        assert_eq!(
            book.standing(Path::new("/work/repo"), LspTrustMode::Never),
            Standing::Trusted
        );
        // A path that only shares a name prefix is not below the folder.
        assert_eq!(book.standing(Path::new("/workshop"), ask), Standing::Ask);
    }

    #[test]
    fn ask_forgets_the_folders_own_decision_and_leaves_the_rest() {
        let mut book = TrustBook::new([
            entry("/work", LspTrust::Trusted),
            entry("/work/a", LspTrust::Denied),
        ]);
        book.set(entry("/work/a", LspTrust::Ask));
        assert_eq!(
            book.standing(Path::new("/work/a"), LspTrustMode::Ask),
            Standing::Trusted
        );
        book.set(entry("/work", LspTrust::Ask));
        assert_eq!(
            book.standing(Path::new("/work/a"), LspTrustMode::Ask),
            Standing::Ask
        );
        assert!(book.entries().is_empty());
    }
}
