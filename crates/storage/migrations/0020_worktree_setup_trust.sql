-- Schema v20: which setup and teardown commands of a repository the user confirmed (slice
-- WORKTREE-SETUP, protocol 0.49, capability `worktree.setup`).
--
-- `.hutan/worktree.toml` is committed with the repository, so whoever can push to it can put
-- commands in it. The commands of the file run only after the user confirmed exactly these:
-- `hash` is the sha-256 (hex) of the file's `setup` and `teardown` lists, `repo_root` the
-- repository's main checkout (canonical). A change to the commands is another hash and asks
-- again. `decided_by` is the client that confirmed (`desktop`, `cli`, ...). Only the user writes
-- here.
CREATE TABLE worktree_setup_trust (
    repo_root      TEXT NOT NULL,
    hash           TEXT NOT NULL,
    decided_at_ms  INTEGER NOT NULL,
    decided_by     TEXT NOT NULL,
    PRIMARY KEY (repo_root, hash)
);
