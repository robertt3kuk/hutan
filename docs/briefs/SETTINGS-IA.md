# SETTINGS-IA — settings in five groups

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SETTINGS-IA`

**Contract ownership:** none (desktop settings sheet; no protocol).

## Context
User 2026-10-28: "settings should look nicer… by examples". UI-RHYTHM (merged) gave rows, cards and nav icons; the information architecture is still ~34 pages. UI-STUDY plan row 8 (`docs/spikes/2026-10-28-ui-study/README.md` §2.3, `mock-settings.svg`) and `docs/spikes/2026-10-20-settings-v2.md` (`@modified` / `@restart` filters).

## Items (in commit order)
1. Regroup the ~34 pages into about 14 pages in 5 groups (General · Agents · Workspace · Data · Advanced) — propose the mapping in the report first (table old → new), with section cards inside a page for what used to be separate pages.
2. Every old page id stays a deep-link anchor (opens the new page scrolled to the section); ⌘K, `hutan ui set`, the Settings search and every in-app link keep working (parity rule: list each moved page and its new place in `docs/design.md`).
3. Search filters `@modified` (changed from default) and `@restart` (needs a restart), as chips under the search field.
4. Docs: `docs/design.md` § Settings sheet.

## Acceptance
GPUI tests: every old page id still opens the right section, search finds every key, `@modified`/`@restart` filters, group order; Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(SETTINGS-IA)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/settingsia/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
