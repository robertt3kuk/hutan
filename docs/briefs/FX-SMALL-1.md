# FX-SMALL-1 — restore memory across restarts, short paths in the Rewind sheet, Vim `G`

Agent: implementer-opus (Claude Opus 5.5)   Depends on: CK-5, FX-VIM-1 merged   Branch: `slice/FX-SMALL-1`

**Contract ownership:** delegated: one additive protocol change, 0.39 → **0.40** (renumber at merge time if another minor lands first), golden fixtures appended.

## Items
1. **`Restored — Undo` survives a restart** (CK-5 report): the wire does not say which checkpoint a restore restored, so only the window that did the restore remembers it. Add `restores: Option<CheckpointId>` (the turn checkpoint a `restore`-kind checkpoint rewound; `through` too if a chain) to `CheckpointInfo`, filled by the daemon from the stored restore row; the desktop derives the footer from `checkpoint.list` instead of window memory. Test: restore, restart desktop (and daemon), the footer still says `Restored — Undo`; undo flips it back.
2. **Short paths in the Rewind sheet** (`~/hutan-scratch/ck5/shots/01-preview-conflict-dark.png`): the header and the honest label repeat the full `/private/var/folders/…/demo` path twice. Show the project name (or `~`-abbreviated path when outside a project) in the header, and `this checkout` in the label sentence with the full path in a tooltip. CLI keeps the full path.
3. **Vim `G` on a file ending with a newline** (FX-VIM-1 report): lands on an empty extra line (Ln 5 for 4 lines); Vim stops at the last real line. Fix in `crates/vim` / the buffer line count used for motions (`G`, `j` at the end, `:$`, `dG`), with a differential fixture recorded from real Vim (`xtask vim-record`) if the harness covers it, else a unit test with the Vim-documented behaviour.

## Acceptance
Per item above; protocol golden fixtures appended; `docs/protocol.md` § Checkpoints. `just check` green; commit per item, messages end `(FX-SMALL-1)`. Fresh `HUTAN_HOME`; no `hutand` left.

## Out of scope
Everything else; `docs/status.md`, `docs/roadmap.md`.
