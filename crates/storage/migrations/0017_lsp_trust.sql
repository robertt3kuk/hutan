-- Schema v17: what the user decided about a folder's language servers (slice E-11b, protocol
-- 0.39, capability `lsp`).
--
-- A language server runs a project's build scripts and procedural macros (rust-analyzer does not
-- sandbox them), so a server starts in a folder only after the user trusted it. `folder` is a
-- canonical absolute path; a decision covers everything below it that has no decision of its
-- own. `decision` is `trusted` or `denied`: forgetting a decision deletes the row. `decided_by`
-- is the client that decided (`desktop`, `cli`, ...). Only the user writes here.
CREATE TABLE lsp_trust (
    folder         TEXT PRIMARY KEY,
    decision       TEXT NOT NULL CHECK (decision IN ('trusted', 'denied')),
    decided_at_ms  INTEGER NOT NULL,
    decided_by     TEXT NOT NULL
);
