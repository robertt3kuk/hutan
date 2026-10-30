-- Schema v24: the runs of the scheduled model digest (slice MAWAS-DIGEST-B, protocol 0.69,
-- ADR 0017 amendment 2026-10-31, `docs/assistant.md` § Catching up).
--
-- One row when a run is over, whatever became of it: `done` (the digest chat holds the answer),
-- `skipped` (the check before the first model call refused the launch, `reason` says why),
-- `failed` or `timeout`. The schedule is judged from the newest `started_ms`, so a run that
-- failed is not tried again before the next period, and the settings page shows the last run
-- and what a run costs (`tokens`). Never a transcript: the answer lives in the chat journal.
-- The newest 50 rows are kept.
CREATE TABLE digest_runs (
    started_ms INTEGER NOT NULL,
    ended_ms   INTEGER NOT NULL,
    outcome    TEXT    NOT NULL,
    reason     TEXT,
    session_id TEXT,
    tokens     INTEGER
);
CREATE INDEX digest_runs_started ON digest_runs (started_ms);
