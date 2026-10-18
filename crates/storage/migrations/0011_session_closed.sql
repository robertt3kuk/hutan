-- Schema v11: closed sessions (slice CL-1, protocol 0.30, capability `session.close`).
--
-- `session.close` ends a session and removes it from the lists, but keeps its row, journal,
-- blocks and history-index records so that `session.reopen` can bring it back. A closed row has
-- the time it was closed in `closed_at_ms`; open sessions have NULL. Listing and restoring skip
-- closed rows.
ALTER TABLE sessions ADD COLUMN closed_at_ms INTEGER;
