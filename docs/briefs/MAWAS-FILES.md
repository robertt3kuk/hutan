# MAWAS-FILES — Mawas's folder is browsable, and its changes can be versioned

Agent: implementer (Claude Sonnet 5.5)   Depends on: SB-ATTACH, MEM-SIDEBAR merged   Branch: `slice/MAWAS-FILES`

**Contract ownership:** delegated for one additive protocol minor (next free after 0.42): `project.git_init {project_id}` (user-only), capability `project.git_init`; golden fixtures appended.

## Context
The user (2026-10-25): "Mawas's directory should be browsable for files, and how do we control versions?". Facts:
- Mawas's Global scope works in the **Home folder** (`~/Hutan`, `crates/daemon/src/paths.rs` `HOME_FOLDER_NAME`; `docs/assistant.md` § Scope: Global sees the Home folder only). Its Workspace scope works in that workspace's projects.
- The Files panel shows nothing at Home: `panels/files.rs` "`None` at Home" (`root()`), and Search/Changes follow the active project only.
- The Home folder is not a git repository, so Changes, checkpoints and *Rewind…* do not work for what Mawas (or a terminal at Home) writes there ("Checkpoints need a git repository", ADR 0019).

## Items (in commit order)
1. **Files at Home**: when the active container is Home, Files (and Search, Problems scope) show the Home folder (`~/Hutan`); the scope chip says `Home`. When **Mawas Home** is the focused center tab, Files/Search follow **Mawas's scope** (Global → Home folder; Workspace → that workspace's active project) and the chip says `Mawas · Global` / `Mawas · <workspace>`. Opening a file from there opens a normal file tab.
2. **"Where Mawas works"** in Mawas Home's header ⋯: *Show Folder in Files* (reveals it in the right dock), *Open Folder in Finder*, *Open Terminal Here*.
3. **Version control for the Home folder** (opt-in): when the Home folder is not a repository, the Changes panel at Home and Mawas's ⋯ offer **Track changes in this folder…** — a confirm sheet explains what happens (`git init`, a `.gitignore` with Hutan's own scratch entries, an initial commit of what is there now with the user's git identity; nothing leaves the machine) — then the daemon runs it (`project.git_init`, user-only; refuses `$HOME`, `/`, a folder inside another repository, a folder over the checkpoint seeding limits). After that, Changes, checkpoints (*Rewind…* on Mawas's turns) and Finish work there like in any repo; Finish without a remote says "commit only" (no push).
4. CLI parity: `hutan project git-init [PROJECT|--home]` with the same refusals and a `[y/N]`.
5. Docs: `docs/assistant.md` (where Mawas works, browsing, versioning), `docs/orchestration.md` (Home folder), `docs/protocol.md`, `docs/design.md` (Files at Home, Mawas ⋯).

## Acceptance
GPUI tests: Files shows the Home folder at Home and follows Mawas's scope while Mawas Home is focused (both scopes), chip text; ⋯ entries; the Track changes flow turns Changes on and a Mawas turn that edits a file gets a checkpoint (fixture agent). Daemon tests: `project.git_init` creates the repo + initial commit, refuses `$HOME`/`/`/nested repo/agent-originated. CLI test. Cua screenshots dark + light: Files at Home, Mawas ⋯, the confirm sheet → `~/hutan-scratch/mawasfiles/shots/`. `just check` green; commit per item, messages end `(MAWAS-FILES)`. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
