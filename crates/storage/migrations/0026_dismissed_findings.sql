-- Schema v26: the findings the user dismissed (slice CHECKS-A, protocol 0.70,
-- `docs/assistant.md` § Checks).
--
-- A finding's id is a hash of its detector, its session, its turn and what it is about, so the
-- same problem has the same id on every read and after a restart of the daemon. Only the id and
-- the time are kept: never the finding's text. Only the user dismisses (`agents.findings.dismiss`
-- is user-only). The newest 2000 rows are kept.
CREATE TABLE dismissed_findings (
    id              TEXT    NOT NULL PRIMARY KEY,
    dismissed_at_ms INTEGER NOT NULL
);
CREATE INDEX dismissed_findings_at ON dismissed_findings (dismissed_at_ms);
