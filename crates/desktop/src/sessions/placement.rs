//! The placement badge of a session (protocol 0.14, `docs/design.md` § session row): `main` for
//! the repository's main checkout, `wt <branch>` for a linked worktree, tinted as a warning
//! while another live agent works in the same checkout.

use hutan_protocol::messages::SessionInfo;
use hutan_protocol::worktree::PlacementKind;

/// What a session's row and tab say about where it works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementBadge {
    /// `main`, `wt <branch>`, or `wt (detached)`; while another live agent works in the same
    /// checkout the words say that instead: `shared checkout` (`wt <branch> · shared`).
    pub text: String,
    /// Another live agent works in the same checkout: the badge is drawn as a warning.
    pub shared: bool,
    /// The checkout's folder, its branch, and the warning, one per line.
    pub tooltip: String,
}

/// The badge of `session`; `None` outside a git checkout.
pub fn placement_badge(session: &SessionInfo) -> Option<PlacementBadge> {
    let placement = session.placement.as_ref()?;
    let text = match (placement.kind, placement.branch.as_deref()) {
        (PlacementKind::Main, _) => "main".to_owned(),
        (PlacementKind::Worktree, Some(branch)) => format!("wt {branch}"),
        (PlacementKind::Worktree, None) => "wt (detached)".to_owned(),
    };
    let mut lines = vec![
        match placement.kind {
            PlacementKind::Main => "Main checkout".to_owned(),
            PlacementKind::Worktree => "Worktree".to_owned(),
        },
        placement.root.display().to_string(),
    ];
    if let Some(branch) = &placement.branch {
        lines.push(format!("Branch {branch}"));
    }
    let others = placement.shared_with.len();
    if others > 0 {
        lines.push(match others {
            1 => "⚠ Another live agent works in this checkout: your edits and its can overwrite each other".to_owned(),
            n => format!(
                "⚠ {n} other live agents work in this checkout: your edits and theirs can overwrite each other"
            ),
        });
    }
    let text = match (others > 0, placement.kind) {
        (false, _) => text,
        (true, PlacementKind::Main) => "shared checkout".to_owned(),
        (true, PlacementKind::Worktree) => format!("{text} · shared"),
    };
    Some(PlacementBadge {
        text,
        shared: others > 0,
        tooltip: lines.join("\n"),
    })
}

/// What a quiet sidebar row says about where its session works, on a second line: the badge of
/// [`placement_badge`] only when the session is in a linked worktree (not the main checkout every
/// row would repeat). A checkout another live agent works in is not a line of its own: the row
/// wears a warning glyph in its trailing slot and keeps its one-line height.
pub fn row_placement_badge(session: &SessionInfo) -> Option<PlacementBadge> {
    let placement = session.placement.as_ref()?;
    (placement.kind == PlacementKind::Worktree)
        .then(|| placement_badge(session))
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hutan_core::{SessionId, SessionKind, SessionState, WorkspaceId};
    use hutan_protocol::worktree::Placement;

    use super::*;

    fn session(placement: Option<Placement>) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind: SessionKind::Terminal,
            title: "t".into(),
            state: SessionState::Running,
            command: vec![],
            cwd: None,
            terminal_id: None,
            created_at_ms: 0,
            agent: None,
            restored: false,
            chat: None,
            placement,
            folder: None,
            recovery: None,
            muted: false,
        }
    }

    fn placement(kind: PlacementKind, branch: Option<&str>, shared: usize) -> Placement {
        Placement {
            kind,
            root: PathBuf::from("/work/repo"),
            branch: branch.map(str::to_owned),
            shared_with: (0..shared).map(|_| SessionId::new()).collect(),
        }
    }

    #[test]
    fn no_placement_no_badge() {
        assert_eq!(placement_badge(&session(None)), None);
    }

    #[test]
    fn the_text_is_main_or_wt_with_the_branch() {
        let badge = |p| placement_badge(&session(Some(p))).unwrap();
        assert_eq!(
            badge(placement(PlacementKind::Main, Some("master"), 0)).text,
            "main"
        );
        assert_eq!(
            badge(placement(PlacementKind::Worktree, Some("feature/x"), 0)).text,
            "wt feature/x"
        );
        assert_eq!(
            badge(placement(PlacementKind::Worktree, None, 0)).text,
            "wt (detached)"
        );
    }

    #[test]
    fn a_row_only_shows_a_placement_that_is_not_the_quiet_default() {
        let badge = |p| row_placement_badge(&session(Some(p))).map(|b| (b.text, b.shared));
        assert_eq!(
            badge(placement(PlacementKind::Main, Some("master"), 0)),
            None
        );
        assert_eq!(
            badge(placement(PlacementKind::Worktree, Some("x"), 0)),
            Some(("wt x".to_owned(), false))
        );
        assert_eq!(
            badge(placement(PlacementKind::Main, Some("master"), 1)),
            None,
            "a shared main checkout is a glyph in the row, not a second line"
        );
        assert_eq!(
            badge(placement(PlacementKind::Worktree, Some("x"), 1)),
            Some(("wt x · shared".to_owned(), true))
        );
        assert_eq!(row_placement_badge(&session(None)), None);
    }

    #[test]
    fn sharing_warns_and_the_tooltip_says_who_and_where() {
        let alone = placement_badge(&session(Some(placement(
            PlacementKind::Main,
            Some("master"),
            0,
        ))))
        .unwrap();
        assert!(!alone.shared);
        assert_eq!(alone.tooltip, "Main checkout\n/work/repo\nBranch master");

        let one = placement_badge(&session(Some(placement(
            PlacementKind::Worktree,
            Some("x"),
            1,
        ))))
        .unwrap();
        assert!(one.shared);
        assert!(
            one.tooltip
                .contains("⚠ Another live agent works in this checkout")
        );
        assert_eq!(one.text, "wt x · shared");
        let two = placement_badge(&session(Some(placement(PlacementKind::Main, None, 2)))).unwrap();
        assert!(
            two.tooltip
                .contains("⚠ 2 other live agents work in this checkout")
        );
        assert_eq!(two.text, "shared checkout");
    }
}
