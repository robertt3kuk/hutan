-- Schema v15: the queue of a chat session (slice UX-QUEUE-STEER, protocol 0.36): messages the
-- user sent while a turn ran, in the order they are sent. Text only, bounded by the daemon
-- (20 messages, 1 MiB of text per session). Removed with the session.
CREATE TABLE chat_queue (
    session_id   TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    item_id      INTEGER NOT NULL,
    position     INTEGER NOT NULL,
    text         TEXT NOT NULL,
    queued_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_id, item_id)
) STRICT, WITHOUT ROWID;
