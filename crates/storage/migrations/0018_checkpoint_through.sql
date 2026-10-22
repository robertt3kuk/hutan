-- Schema v18: the last checkpoint of the chain a restore rewound (`checkpoint.restore`'s
-- `through`, slice FX-SMALL-1). `parent_id` already holds the first one; together they are the
-- restore's `restores` / `restores_through` on the wire (protocol 0.40), so every window, and a
-- restarted one, knows which turns stand restored. NULL for a turn and for a restore of one
-- checkpoint.
ALTER TABLE checkpoints ADD COLUMN through_id TEXT;
