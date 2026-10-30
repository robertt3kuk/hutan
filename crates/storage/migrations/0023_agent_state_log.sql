-- Schema v23: the log of agents' states (slice AGENTS-FLOW, protocol 0.66, ADR 0025 decision 3,
-- `docs/assistant.md` § Flow).
--
-- One row each time the state an agent reports about itself changes: an `agent.report` of a hook
-- or the status a terminal agent announces (OSC 7501). It is the only record of how long a
-- *terminal* agent waited for the user: the daemon keeps nothing but the current state of such
-- an agent. A row holds the state word, the source that said it, what a blocked agent waits for
-- (`kind`) and, for a wait or an error, a short message already redacted by the history index's
-- redactor and cut to 120 characters. Never output, never a transcript. Rows older than 14 days
-- are deleted as new ones are written, and the whole table is emptied when the daemon's
-- `[agents] state_log` setting is turned off.
CREATE TABLE agent_state_log (
    session_id TEXT    NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    at_ms      INTEGER NOT NULL,
    source     TEXT    NOT NULL,
    state      TEXT    NOT NULL,
    kind       TEXT,
    message    TEXT
);
CREATE INDEX agent_state_log_by_session ON agent_state_log (session_id, at_ms);
CREATE INDEX agent_state_log_at ON agent_state_log (at_ms);
