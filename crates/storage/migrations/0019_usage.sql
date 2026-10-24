-- Schema v19: usage and limits (protocol 0.45, slice USAGE-LIMITS, `docs/agents.md` § Usage).
--
-- `usage_turns`: one row per turn Hutan ran that reported tokens or a cost, written when the
-- turn ends; `usage.summary` sums them by day and week. `usage_limits`: the newest limit
-- window each harness reported (the 5-hour and weekly windows of a plan): a percentage, a
-- length and a reset time. Neither table holds anything that identifies an account or a
-- credential (ADR 0015): the strings are a harness id, a provider id, a model id, a window id
-- and a plan name, all short identifiers. No foreign keys: the totals outlive the session.
CREATE TABLE usage_turns (
    session_id            TEXT NOT NULL,
    turn_id               TEXT NOT NULL,
    at_ms                 INTEGER NOT NULL,
    harness               TEXT NOT NULL,
    provider              TEXT NOT NULL,
    model                 TEXT,
    input_tokens          INTEGER,
    cached_input_tokens   INTEGER,
    output_tokens         INTEGER,
    reasoning_output_tokens INTEGER,
    total_tokens          INTEGER,
    cost_usd              REAL,
    PRIMARY KEY (session_id, turn_id)
);
CREATE INDEX usage_turns_at ON usage_turns (at_ms);

CREATE TABLE usage_limits (
    harness         TEXT NOT NULL,
    provider        TEXT NOT NULL,
    window_id       TEXT NOT NULL,
    label           TEXT NOT NULL,
    used_percent    REAL NOT NULL,
    window_minutes  INTEGER,
    resets_at_ms    INTEGER,
    plan            TEXT,
    observed_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (harness, provider, window_id)
);
