-- Schema v12: grants of the control gate (slice MW-1, protocol 0.31, capability `control.grants`).
--
-- When Mawas (the assistant) asks Hutan to change something, the user may answer "allow for this
-- chat" or "always allow this tool in this workspace"; the daemon stores that answer here and
-- consults it before it asks again. `tool` is the request's grant key (`session.rename`,
-- `ui.run:<action>`, `ui.set_setting:<namespace>`). A grant has either a `workspace_id` (every
-- Mawas chat of that workspace) or a `chat_session_id` (that chat only), never both. Destructive
-- requests are never covered by a grant. `created_by` is the client that answered (`desktop`,
-- `cli`, ...). No foreign keys: a closed chat's grant stays listed until the user revokes it.
CREATE TABLE control_grants (
    id              INTEGER PRIMARY KEY,
    tool            TEXT NOT NULL,
    workspace_id    TEXT,
    chat_session_id TEXT,
    created_ms      INTEGER NOT NULL,
    created_by      TEXT NOT NULL,
    CHECK ((workspace_id IS NULL) <> (chat_session_id IS NULL))
);
CREATE INDEX control_grants_tool ON control_grants (tool);
