-- Schema v21: limit windows per account (protocol 0.50, slice ACCOUNTS-MULTI, `docs/agents.md`
-- § Several accounts per harness). `usage_limits` keeps the newest report of each window; with
-- several accounts of one harness the same window (`5h`) exists once per account, so the
-- account joins the key. `account` is the name of a managed account, or '' for the harness's own
-- login (every row there was until now). Nothing that identifies a person or a credential is
-- stored: the name is the short identifier the user gave the account.
CREATE TABLE usage_limits_v21 (
    harness         TEXT NOT NULL,
    account         TEXT NOT NULL DEFAULT '',
    provider        TEXT NOT NULL,
    window_id       TEXT NOT NULL,
    label           TEXT NOT NULL,
    used_percent    REAL NOT NULL,
    window_minutes  INTEGER,
    resets_at_ms    INTEGER,
    plan            TEXT,
    observed_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (harness, account, provider, window_id)
);
INSERT INTO usage_limits_v21 (harness, account, provider, window_id, label, used_percent,
                              window_minutes, resets_at_ms, plan, observed_at_ms)
    SELECT harness, '', provider, window_id, label, used_percent, window_minutes, resets_at_ms,
           plan, observed_at_ms
    FROM usage_limits;
DROP TABLE usage_limits;
ALTER TABLE usage_limits_v21 RENAME TO usage_limits;
