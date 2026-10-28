# BEST-OF — Superset-like style, fork from any point, multi-folder sessions, context view

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/BEST-OF`

**Contract ownership:** none — research/design spike, docs only.

## Context
User 2026-10-28 (after `docs/spikes/2026-10-28-ui-study/README.md` and `docs/spikes/2026-10-28-insight.md`):
- "Don't copy Orca's counters; another format is fine. The main thing: make **our icons and cards nicer**, a pleasant style **like Superset**."
- "No lines-of-code counts in the status/cards — we see them later anyway (maybe as an option). The main thing is that the **session name** comes through."
- "Inside a workspace, be able to **open another directory** and maybe **combine it into the session**."
- "On the right, see **how the context is filled** where possible, and **fork the session from any point** (I think we may already have it — we don't)."
- "Improve everything: research ours against the best."
A Superset product shot is at `~/hutan-scratch/private/ref-superset-layout.png` (keep out of the repo; describe in words).

## Deliverable
`docs/spikes/2026-10-28-best-of/README.md` (+ our own SVG mocks) with:
1. **Superset's visual language** (from its site, docs, public repo/source if available, the shot): typography (UI font, mono usage, sizes, weights, uppercase group labels), colour use (dots, muted text, accents), density (row heights, paddings), icon style (stroke width, size, set), cards, right panel tabs. Compare with Hutan today (scratch build, Cua) and give a **concrete style spec for Hutan** (tokens, row anatomy for agent/session/worktree rows, icon set choice — Phosphor stays? weight/size), no copying of trademarks/logos.
2. **Fork from any point**: what each harness supports (OMP ACP `sessionCapabilities.fork`, `omp --fork`; Claude Code `--fork-session` / `/fork`?; Codex `thread/fork` or equivalent) — verify from docs/`--help`/source/recorded fixtures; how Cursor, Claude desktop, Codex app, Zed, Orca, Superset expose "fork/branch from this message"; how it fits Hutan's checkpoints (ADR 0019: files at that turn) — fork = new chat at turn N + optional worktree restored to that turn's checkpoint.
3. **Multi-folder sessions**: harness support for extra directories (Claude `--add-dir`, Codex `--add-dir`/`sandbox_workspace_write.writable_roots`, OMP `workspace.additionalDirectories`/ACP `additionalDirectories`?), Hutan workspaces/projects/roots (EX-8 stacked roots), daemon file-service roots and Contain write roots; UX for "open another folder in this workspace" and "add it to this session".
4. **Context view**: refine the insight panel (from the insight spike) into the right-dock design in the new style.
5. **Ours vs best** short scorecard on the remaining weakest areas and the **slice plan** in commit order with protocol impact; mark decisions already taken: no +/- line counts by default (option), usage shown in our own format (not Orca's meter row), session name is the top priority (AGENT-TERM-1 running).

## Rules
- Work only in `../hutan-wt/BEST-OF`; screens under `~/hutan-scratch/bestof/`; no product code; don't build the workspace if a debug build exists in another worktree (else one `cargo build -p hutan-desktop -p hutan-daemon`); scratch `HUTAN_HOME`; never touch the user's Hutan or harness credentials/sessions; never pkill by name; Cua: switch input to ABC, restore Russian. Don't edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`. Commit on `slice/BEST-OF`; report in ≤15 lines.
