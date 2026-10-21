-- Schema v14: checkpoints of agent edits (ADR 0019, slice CK-2).
--
-- One row per checkpoint: an agent turn of a chat session (its start and end snapshot) or, from
-- CK-4 on, a restore. The snapshots themselves are tree commits in the repository at
-- `common_dir`; `ref` names the one hidden ref that keeps a checkpoint's commits
-- (`refs/hutan/checkpoints/<session>/<turn>`, the end commit, whose parent is `start_commit`;
-- `<turn>-start` while the turn runs). A turn that changed nothing, failed, or whose folder
-- cannot have checkpoints has no ref, and `state` and `reason` say why. Rows are rebuildable
-- from the refs' commit trailers (`Hutan-Session`, `Hutan-Turn`, `Hutan-Kind`,
-- `Hutan-Checkpoint`); the daemon reconciles both at start. `skipped_json` lists the paths left
-- out and why (never their contents). No foreign keys: the checkpoints of a closed chat stay
-- until retention drops them.
CREATE TABLE checkpoints (
    id            TEXT PRIMARY KEY,
    session_id    TEXT NOT NULL,
    turn_id       TEXT,
    kind          TEXT NOT NULL CHECK (kind IN ('turn', 'restore')),
    state         TEXT NOT NULL
        CHECK (state IN ('running', 'ok', 'late', 'unchanged', 'failed', 'off', 'unsupported')),
    reason        TEXT,
    checkout      TEXT,
    common_dir    TEXT,
    ref           TEXT,
    commit_id     TEXT,
    start_commit  TEXT,
    tree          TEXT,
    parent_id     TEXT,
    created_ms    INTEGER NOT NULL,
    ended_ms      INTEGER,
    files_changed INTEGER NOT NULL DEFAULT 0,
    lines_added   INTEGER NOT NULL DEFAULT 0,
    lines_removed INTEGER NOT NULL DEFAULT 0,
    bytes         INTEGER NOT NULL DEFAULT 0,
    skipped_json  TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX checkpoints_session ON checkpoints (session_id, created_ms);
CREATE INDEX checkpoints_repository ON checkpoints (common_dir, created_ms);
