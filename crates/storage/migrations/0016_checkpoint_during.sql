-- Schema v16: the files the user saved while a checkpoint's turn ran (ADR 0019, slice CK-4).
--
-- A JSON array of paths relative to the checkout, at most 1 000. A restore lists such a file as
-- a conflict (`during`): the turn's end holds the user's edit too, and restoring it would take
-- that edit back. Paths only, never contents.
ALTER TABLE checkpoints ADD COLUMN during_json TEXT NOT NULL DEFAULT '[]';
