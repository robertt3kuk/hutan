-- Schema v13: Mawas memory (slice MW-2, protocol 0.32, capability `memory`).
--
-- Curated, revisioned knowledge the user approved (profile, fact, note), in the global scope or
-- in one workspace. `memory_entries` is the current state of an entry; `memory_revisions` is its
-- immutable history (an update, an archive and a restore each add a revision; triggers refuse to
-- change or delete one). `operation_id` is unique: repeating a request returns the receipt of
-- the revision it created. `memory_scopes.revision` counts the changes of a scope (what a
-- receipt reports and what context deltas compare). `memory_proposals` holds what an agent
-- asked to change until the user answers (its `id` is also the id of the approval). A proposal
-- that cannot be applied because the entry moved on is kept in state `conflict`.
-- `memory_snapshots` is exactly what Mawas's backend was given (the context at a conversation's
-- start, and each later delta), with the manifest of the entries it listed.
-- No foreign keys to workspaces or sessions: a closed chat's snapshots and a deleted workspace's
-- notes stay until the user removes them.
CREATE TABLE memory_scopes (
    scope_key TEXT PRIMARY KEY,          -- 'global' or 'workspace:<id>'
    revision  INTEGER NOT NULL
);

CREATE TABLE memory_entries (
    id               TEXT PRIMARY KEY,
    scope_kind       TEXT NOT NULL CHECK (scope_kind IN ('global', 'workspace')),
    workspace_id     TEXT,
    category         TEXT NOT NULL CHECK (category IN ('profile', 'fact', 'note')),
    current_revision INTEGER NOT NULL,
    state            TEXT NOT NULL CHECK (state IN ('active', 'archived')),
    created_ms       INTEGER NOT NULL,
    updated_ms       INTEGER NOT NULL,
    CHECK ((scope_kind = 'workspace') = (workspace_id IS NOT NULL))
);
CREATE INDEX memory_entries_scope ON memory_entries (scope_kind, workspace_id, state);

CREATE TABLE memory_revisions (
    entry_id        TEXT NOT NULL REFERENCES memory_entries (id),
    revision        INTEGER NOT NULL,
    operation       TEXT NOT NULL CHECK (operation IN ('create', 'update', 'archive', 'restore')),
    body            TEXT NOT NULL,
    content_hash    TEXT NOT NULL,
    provenance_json TEXT NOT NULL,
    author_kind     TEXT NOT NULL CHECK (author_kind IN ('user', 'agent')),
    approval_id     TEXT,
    operation_id    TEXT NOT NULL UNIQUE,
    scope_revision  INTEGER NOT NULL,
    created_ms      INTEGER NOT NULL,
    PRIMARY KEY (entry_id, revision)
);

CREATE TRIGGER memory_revisions_no_update BEFORE UPDATE ON memory_revisions
BEGIN
    SELECT RAISE(ABORT, 'memory revisions are immutable');
END;
CREATE TRIGGER memory_revisions_no_delete BEFORE DELETE ON memory_revisions
BEGIN
    SELECT RAISE(ABORT, 'memory revisions are immutable');
END;

CREATE TABLE memory_proposals (
    id                     TEXT PRIMARY KEY,
    operation_id           TEXT NOT NULL UNIQUE,
    scope_kind             TEXT NOT NULL CHECK (scope_kind IN ('global', 'workspace')),
    workspace_id           TEXT,
    operation              TEXT NOT NULL CHECK (operation IN ('create', 'update', 'archive', 'restore')),
    entry_id               TEXT,
    expected_revision      INTEGER,
    category               TEXT NOT NULL CHECK (category IN ('profile', 'fact', 'note')),
    body                   TEXT NOT NULL,
    provenance_json        TEXT NOT NULL,
    state                  TEXT NOT NULL CHECK (state IN ('pending', 'applied', 'rejected', 'conflict', 'cancelled')),
    origin_chat_session_id TEXT,
    created_ms             INTEGER NOT NULL,
    resolved_ms            INTEGER
);
CREATE INDEX memory_proposals_state ON memory_proposals (state, created_ms);

CREATE TABLE memory_snapshots (
    id              TEXT PRIMARY KEY,
    chat_session_id TEXT NOT NULL,
    backend         TEXT NOT NULL,
    manifest_json   TEXT NOT NULL,
    rendered        TEXT NOT NULL,
    content_hash    TEXT NOT NULL,
    created_ms      INTEGER NOT NULL
);
CREATE INDEX memory_snapshots_chat ON memory_snapshots (chat_session_id, created_ms);
