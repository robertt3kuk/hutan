//! Which checkpoints retention drops (ADR 0019 Decision 7, spike §3.7). Pure: the caller
//! deletes the refs and rows.
//!
//! Over the checkpoints of one repository (or of the sessions outside git), oldest first:
//! 1. a session whose newest checkpoint is older than `keep_days` loses all of them;
//! 2. a session keeps its newest `keep_turns` turn checkpoints (restores do not count);
//! 3. while the checkpoints with a ref record more than `max_repo_bytes`, or more than
//!    `max_repo_refs` refs remain, the oldest go: turns first, restores (the pre-restore
//!    state that Undo needs) only when no turn is left to drop.
//!
//! A running turn is never dropped.

use std::collections::{HashMap, HashSet};

use hutan_core::{CheckpointId, CheckpointKind, CheckpointState, SessionId};
use hutan_storage::CheckpointRow;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// The bounds retention keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub keep_turns: usize,
    pub keep_days: u64,
    pub max_repo_bytes: u64,
    pub max_repo_refs: usize,
}

/// The checkpoints of `rows` (one repository's) to drop at `now_ms`, oldest first.
pub fn plan(rows: &[CheckpointRow], now_ms: u64, policy: &Policy) -> Vec<CheckpointId> {
    let mut order: Vec<&CheckpointRow> = rows.iter().collect();
    order.sort_by_key(|row| (row.created_ms, row.id));
    let droppable = |row: &CheckpointRow| row.state != CheckpointState::Running;
    let mut dropped: HashSet<CheckpointId> = HashSet::new();

    // 1. Sessions untouched for `keep_days`.
    let mut newest: HashMap<SessionId, u64> = HashMap::new();
    for row in &order {
        let at = newest.entry(row.session_id).or_default();
        *at = (*at).max(row.created_ms);
    }
    let horizon = now_ms.saturating_sub(policy.keep_days.saturating_mul(DAY_MS));
    for row in &order {
        if newest[&row.session_id] < horizon && droppable(row) {
            dropped.insert(row.id);
        }
    }

    // 2. The newest `keep_turns` turns of each session.
    let mut seen: HashMap<SessionId, usize> = HashMap::new();
    for row in order.iter().rev() {
        if row.kind != CheckpointKind::Turn || dropped.contains(&row.id) {
            continue;
        }
        let count = seen.entry(row.session_id).or_default();
        *count += 1;
        if *count > policy.keep_turns && droppable(row) {
            dropped.insert(row.id);
        }
    }

    // 3. Bytes and refs of the repository: turns oldest first, then restores.
    let kept = |dropped: &HashSet<CheckpointId>| {
        order
            .iter()
            .filter(|row| row.reference.is_some() && !dropped.contains(&row.id))
            .fold((0u64, 0usize), |(bytes, refs), row| {
                (bytes.saturating_add(row.bytes), refs + 1)
            })
    };
    let (mut bytes, mut refs) = kept(&dropped);
    for kind in [CheckpointKind::Turn, CheckpointKind::Restore] {
        for row in &order {
            if bytes <= policy.max_repo_bytes && refs <= policy.max_repo_refs {
                break;
            }
            if row.kind != kind
                || row.reference.is_none()
                || dropped.contains(&row.id)
                || !droppable(row)
            {
                continue;
            }
            dropped.insert(row.id);
            bytes = bytes.saturating_sub(row.bytes);
            refs -= 1;
        }
    }

    order
        .iter()
        .filter(|row| dropped.contains(&row.id))
        .map(|row| row.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 100 * DAY_MS;

    fn policy() -> Policy {
        Policy {
            keep_turns: 100,
            keep_days: 30,
            max_repo_bytes: u64::MAX,
            max_repo_refs: usize::MAX,
        }
    }

    fn row(session: SessionId, kind: CheckpointKind, at: u64, bytes: u64) -> CheckpointRow {
        let mut row = CheckpointRow::new(CheckpointId::new(), session, kind, CheckpointState::Ok, at);
        row.reference = Some(format!("refs/hutan/checkpoints/{session}/{at}"));
        row.bytes = bytes;
        row
    }

    fn ids(rows: &[&CheckpointRow]) -> Vec<CheckpointId> {
        rows.iter().map(|r| r.id).collect()
    }

    #[test]
    fn nothing_goes_within_the_bounds() {
        let s = SessionId::new();
        let rows = vec![row(s, CheckpointKind::Turn, NOW - 1, 10)];
        assert!(plan(&rows, NOW, &policy()).is_empty());
    }

    #[test]
    fn a_session_keeps_its_newest_turns_and_every_restore() {
        let (s, other) = (SessionId::new(), SessionId::new());
        let rows = vec![
            row(s, CheckpointKind::Turn, NOW - 50, 0),
            row(s, CheckpointKind::Restore, NOW - 45, 0),
            row(s, CheckpointKind::Turn, NOW - 40, 0),
            row(other, CheckpointKind::Turn, NOW - 35, 0),
            row(s, CheckpointKind::Turn, NOW - 30, 0),
            row(s, CheckpointKind::Turn, NOW - 20, 0),
        ];
        let policy = Policy {
            keep_turns: 2,
            ..policy()
        };
        assert_eq!(plan(&rows, NOW, &policy), ids(&[&rows[0], &rows[2]]));
    }

    #[test]
    fn a_session_untouched_for_the_days_goes_whole_and_a_running_turn_stays() {
        let (old, fresh) = (SessionId::new(), SessionId::new());
        let mut running = row(old, CheckpointKind::Turn, NOW - 40 * DAY_MS, 0);
        running.state = CheckpointState::Running;
        let rows = vec![
            row(old, CheckpointKind::Turn, NOW - 45 * DAY_MS, 0),
            row(old, CheckpointKind::Restore, NOW - 31 * DAY_MS, 0),
            running,
            row(fresh, CheckpointKind::Turn, NOW - 45 * DAY_MS, 0),
            row(fresh, CheckpointKind::Turn, NOW - DAY_MS, 0),
        ];
        assert_eq!(plan(&rows, NOW, &policy()), ids(&[&rows[0], &rows[1]]));
    }

    #[test]
    fn bytes_and_refs_drop_the_oldest_turns_first_and_restores_last() {
        let s = SessionId::new();
        let mut unchanged = row(s, CheckpointKind::Turn, NOW - 70, 0);
        unchanged.reference = None;
        let rows = vec![
            row(s, CheckpointKind::Restore, NOW - 80, 50),
            unchanged,
            row(s, CheckpointKind::Turn, NOW - 60, 50),
            row(s, CheckpointKind::Turn, NOW - 50, 50),
            row(s, CheckpointKind::Turn, NOW - 40, 50),
        ];
        // 200 bytes recorded, at most 120: the two oldest turns with a ref go, the older
        // restore stays.
        let bytes = Policy {
            max_repo_bytes: 120,
            ..policy()
        };
        assert_eq!(plan(&rows, NOW, &bytes), ids(&[&rows[2], &rows[3]]));
        // Four refs, at most one: every turn goes before the restore does.
        let refs = Policy {
            max_repo_refs: 1,
            ..policy()
        };
        assert_eq!(
            plan(&rows, NOW, &refs),
            ids(&[&rows[2], &rows[3], &rows[4]])
        );
        // At most none: the restore goes last.
        let none = Policy {
            max_repo_refs: 0,
            ..policy()
        };
        assert_eq!(
            plan(&rows, NOW, &none),
            ids(&[&rows[0], &rows[2], &rows[3], &rows[4]])
        );
    }
}
