# FX-AGENT-FILES — Files touched / Changed by agents miss a file the agent changed

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-AGENT-FILES`

**Contract ownership:** none (bug fix; no protocol change expected — report first if one is needed).

## Context
User 2026-11-02 (0.3.5, while recording a demo): an agent changed three files; **Files touched** (insight / Context panel) and **Changed by agents** (Files panel) both list only two of them — `safety.rs` is missing — while the Changes view and the turn review packet correctly show all three. In the user's screenshot (kept outside the repo) *Changed by agents 2* lists **`Cargo.toml` twice** (OMP · 1 hunk each) — likely two different `Cargo.toml` paths shown by basename, or one file counted twice and another dropped.

## Change
Reproduce with fixture journals (OMP, Claude Code, Codex shapes; an edit tool, a write tool and a patch/apply; same basename in two folders; a file edited in two turns), find why the insight reducer's files and the Files panel's agent-changes list disagree with the Changes view / review packet (dedup key? basename vs path? only the first item of a multi-file tool call? an event kind not folded?), and fix at the root so all four surfaces agree on the set of files. Rows with the same basename show enough of the path to tell them apart.

## Acceptance
Regression tests at the reducer/daemon level and a GPUI test for both panels (three files incl. two same-named in different folders → three distinct rows); Cua screenshot dark + light; `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-AGENT-FILES)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxagentfiles/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
