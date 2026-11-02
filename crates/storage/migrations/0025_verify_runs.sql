-- Schema v25: the verification runs of `agents.verify` (slice CHECKS-B, protocol 0.71,
-- capability `agents.verify`, `docs/assistant.md` § Checks › Verification).
--
-- One row per run, written when the run starts (status `running`) and completed when it ends:
-- the command Hutan ran in the session's folder, why that command, how it ended (`passed`,
-- `failed`, `timed_out`, `cancelled`, `error`), the exit code, the duration and the last 8 KiB
-- of the output, redacted by the daemon before it gets here. A row left `running` by a daemon
-- that stopped is completed as `cancelled` at the next start. The newest 20 rows of a session
-- are kept; a session's rows go with it.
CREATE TABLE verify_runs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id       TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    command          TEXT    NOT NULL,
    cwd              TEXT    NOT NULL,
    source           TEXT    NOT NULL,
    why              TEXT    NOT NULL,
    status           TEXT    NOT NULL,
    exit_code        INTEGER,
    signal           INTEGER,
    started_ms       INTEGER NOT NULL,
    ended_ms         INTEGER,
    duration_ms      INTEGER,
    output_tail      TEXT    NOT NULL DEFAULT '',
    output_truncated INTEGER NOT NULL DEFAULT 0,
    requested_by     TEXT    NOT NULL,
    forced           INTEGER NOT NULL DEFAULT 0,
    error            TEXT
);
CREATE INDEX verify_runs_by_session ON verify_runs (session_id, started_ms);
