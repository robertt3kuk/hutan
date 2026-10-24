# Assistant, memory and knowledge needs (2026-10-25)

Repos: `anthropics/claude-code`, `openai/codex`, `cline/cline`, `RooCodeInc/Roo-Code` (**archived** 2026-05-15, `gh repo view` `isArchived: true`, so historical only), `continuedev/continue`.

## Method (reproducible)

- `gh issue list --search "sort:reactions-+1-desc"` returned only 502/504 from GraphQL, and the background fetches wrote empty files. I used the REST search instead: `gh api -X GET search/issues -f q='repo:<repo> is:issue <term>' -f sort=reactions -f order=desc -f per_page=30`.
- Terms: `memory`, `CLAUDE.md`, `AGENTS.md`, `"auto memory"`, `"/memory"`, `"project knowledge"`, `notes`, `rules`, `"memory bank"`, `"custom instructions"`, `knowledge`, `"context file"`, `"workspace folder"`.
- Each candidate was checked with `gh api repos/<repo>/issues/<n>` for state, `state_reason`, reactions and comment count. The 👍 figure is `reactions["+1"]`.
- Roo: the local top-100 list (`/tmp/ami_RooCodeInc_Roo-Code.json`) was filtered by title. Search returned nothing after the search rate limit (30/min) was hit.
- Hutan status comes from docs and code only; the app was not run. Dates are 2026-10-25.

## 1. Top needs (memory, instructions and knowledge)

Category list is the fixed one from the brief. "Hutan status" is based on `docs/assistant.md` § Memory unless noted.

| # | Need (user's words) | Repos / issues (👍 · comments · state) | Category | Hutan status |
|---|---|---|---|---|
| 1 | "Support AGENTS.md" as a shared, tool-agnostic instruction file | claude-code #6235 (👍5189 · 409 · closed-completed 2026-08-17); #31005 (👍423 · 37 · open); #34235 (👍114 · dup); cline #5033 (👍16 · 18 · closed-completed); continue #6716 (👍18 · 10 · closed-completed); Roo #5966 (👍6) | integrations | **missing**: no user-facing instruction-file surface in `docs/` [INFERENCE] |
| 2 | "Store project memory files in project-local .claude folder" (versionable) | claude-code #25947 (👍45 · 10 · open) | memory | **missing**: memory lives in SQLite (`memory_entries`), not files |
| 3 | "Portable project memory across machines" | claude-code #25739 (👍25 · 28 · open) | memory | **missing**: no export; CLI has `ls/add/edit/archive/restore/history/proposals` only |
| 4 | Configurable memory location; share memory across git worktrees | claude-code #28276 (👍24 · 15 · closed-completed); #24382 (👍19 · 8 · closed-completed) | memory | **partial**: per-workspace scope exists; no worktree-shared scope documented |
| 5 | "Support shared memory across multiple projects" / user-level scope | claude-code #39195 (👍14 · 10 · open); #83831 (👍0 · 2 · open, 2026-08-04); #7075 (👍12 · 32 · closed-completed) | memory | **has**: `global` vs `workspace` scopes (§ Memory, "What is stored") |
| 6 | "User-editable global memory for Codex preferences" | openai/codex #31763 (👍0 · 2 · open, 2026-07-09) | memory | **has**: global `profile` entries, edited inline on the Memory page and via `hutan memory edit` (design.md § Memory) |
| 7 | "CLAUDE.md rules consistently ignored" / "forgets CLAUDE.md after compaction" | claude-code #2544 (👍41 · 34 · open); #6354 (👍30 · 24 · open); #33603 (👍15 · 20 · open) | memory | **partial**: snapshot injected at conversation start, deltas at turn boundaries (§ "What a conversation is given"); compaction survival not documented |
| 8 | Nested / subdirectory instructions load, and can be excluded | claude-code #2571 (👍51 · 30 · closed not_planned); #20880 (👍28 · 11 · closed-completed); #90450 (👍49 · 25 · open, 2026-08-28); #26489 (👍50 · 17 · open) | memory | **missing**: scopes are global/workspace only, no path-scoped memory |
| 9 | Org-wide shared instructions ("shared CLAUDE.md via GitHub org") | claude-code #14467 (👍49 · 4 · open) | memory | **missing**: memory is local (§ Memory, "locally") |
| 10 | Agent reads memory from claude.ai / ChatGPT | claude-code #14228 (👍39 · 16 · open); codex #51940 (👍0 · 1 · open, 2026-10-08); #52511 (👍0 · 1 · open, 2026-10-09) | integrations | **missing** by design: not a secret store, local-only (§ Rules) |
| 11 | Per-run switch to skip the global AGENTS.md | codex #55157 (👍0 · 0 · open, 2026-10-19) | integrations | **missing** [INFERENCE]: no Codex launch option for global instructions in docs |
| 12 | "Writing memory" with no diff: cannot see what changed | claude-code #23176 (👍12 · 12 · closed-completed 2026-02-17) | memory | **partial**: revisions with SHA-256 and `hutan memory history`; no diff view documented |
| 13 | Memory file limits should be documented (200-line truncation) | claude-code #25006 (👍1 · 9 · closed not_planned) | memory | **has**: caps stated in docs: 200 entries, 2 KiB body, 8 KiB snapshot (§ Rules) |
| 14 | An "advanced memory tool" the agent can save to and recall from | claude-code #87 (👍42 · 12 · closed not_planned 2026-02-01) | memory | **has**: `memory.propose` with user approval (§ Memory) |
| 15 | Dedupe repeated learnings during consolidation | codex #29818 (👍0 · 1 · closed-completed 2026-06-24) | memory | **partial**: user approves each entry; no dedupe, 200-entry cap |
| 16 | Long-term memory store (mem0-like) for the agent | Roo #3489 (👍7 · 11 · closed not_planned 2025-09-16); #3312 "memory bank" (👍5 · 11 · closed not_planned; "memory banks don't work without significant manual maintenance") | memory | **has** (curated, not automatic) |
| 17 | Memory re-read every turn drains quota (cache reads) | claude-code #24147 (👍15 · 17 · open 2026-02-08) | cost/usage | **partial**: one snapshot per conversation, plus a delta only at a turn boundary where memory changed (`docs/assistant.md` § "Deltas"); per-turn cost not measured [INFERENCE] |
| 18 | "Orphaned memory files" written outside the memory index | claude-code #24474 (👍0 · 2 · closed not_planned) | memory | **has**: one store, no stray files |
| 19 | Configurable name/location for the instructions file ("CLAUDE.md") | claude-code #1091 (👍39 · 39 · closed-completed); #995 (👍40 · 19 · closed not_planned) | other | **missing** (Hutan has no named instruction file) |
| 20 | Notes/scratch folder that belongs to no project | Not in these repos; Hutan roadmap item, see § below | other | **has**: `workspaces.home_folder` setting, Home workspace (`crates/desktop/src/workbench/settings_labels.rs:845`, `docs/roadmap.md:128`) |

Rows 21–25 were not added. The remaining candidates in these repos are unverified, duplicates, or about bugs with no knowledge-feature angle. Cline and Continue add only the AGENTS.md row (#5033, #6716).

## 2. Five most important missing needs

1. **Project instruction/memory files in the repo (AGENTS.md or versionable markdown)**: #6235 (👍5189), #25947 (👍45). Users want these in git, reviewable and shared. Hutan's memory is SQLite-only, so it cannot appear in a PR or be shared with a team. Its only history is `hutan memory history`.
2. **Export and portability**: #25739 (👍25 · 28 comments). There is no export; the CLI lists no export verb. This also blocks backup and move between machines.
3. **Team or org-wide shared instructions**: #14467 (👍49). Memory is local-only by design, so a team cannot share curated knowledge.
4. **One instruction file read by every agent**: #6235, cline #5033, continue #6716, Roo #5966. Hutan drives Codex, Claude Code and OMP, and no file it manages is read by all three.
5. **Path-scoped and nested instruction loading, with exclusions**: #90450 (👍49), #26489 (👍50), #20880 (👍28). Hutan has only global and workspace scopes, so a rule that applies to one subtree cannot be expressed.

## 3. Evidence gaps

- The GraphQL `gh issue list` path returned 502/504 and the background jobs wrote empty files. All ranking here is from REST search, which caps results at 30 per query. The search rate limit (30/min) was hit mid-run, so Cline and Roo search terms are incomplete.
- Roo is archived (2026-05-15). Its data is a local title filter over the top-100 issues, not a fresh query, and its memory issues have few reactions.
- Cline and Continue returned mostly unrelated matches (`docs`, `prompts`, `rules` in unrelated bugs). Only the AGENTS.md requests are relevant.
- Claude #6235 shows `closed-completed` on 2026-08-17, and comments from 2026-09-18 onward say AGENTS.md support shipped. That is from comments and a linked press item, not the official changelog [INFERENCE]; I did not verify the changelog.
- Codex items #51940, #52511 and #55157 are from October 2026 and have 0–1 reactions, so their weight is uncertain.
- Hutan status is from `docs/assistant.md`, `design.md`, `roadmap.md` and `settings_labels.rs`. It was not checked against a running build.
- Earlier spikes (`docs/spikes/2026-10-21-community-opinions.md`, `docs/spikes/2026-10-21-competitor-ux/`) were used only for context. This note adds the gh-verified counts for those repos.
