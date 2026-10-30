-- Schema v22: target-bound grants and message origin (slice MAWAS-GATE, protocol 0.62,
-- capability `control.grants.target`, ADR 0025).
--
-- `control_grants.target_session_id`: the agent a grant is bound to. The requests that act on
-- another agent (`chat.send`, `chat.send_now`, `chat.queue.add`, `chat.interrupt`,
-- `agent.interrupt`, `task.review`) are covered only by a grant of the Mawas chat that has the
-- target set; a row without one (every grant written before this schema) never covers them. A
-- target grant has `chat_session_id` set and `workspace_id` NULL, so the existing CHECK holds.
ALTER TABLE control_grants ADD COLUMN target_session_id TEXT;

-- `chat_queue.origin_json`: who wrote a queued message when it was not the user (a message
-- Mawas queued with the user's approval): the JSON of `MessageOrigin`, NULL for the user's own.
ALTER TABLE chat_queue ADD COLUMN origin_json TEXT;
