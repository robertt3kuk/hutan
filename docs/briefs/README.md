# Slice briefs

A brief is the complete, self-contained prompt for an implementer agent (`.omp/agents/implementer.md`, see `process.md` § Implementer agents). It is committed **before** launch so any session can relaunch or review against it.

File name = slice ID: `docs/briefs/<slice>.md`. Template:

```markdown
# <slice>: <title>

Agent: implementer   Depends on: <slices/commits>   Branch: slice/<slice>

## Context
Read AGENTS.md, then: <exact docs/ADRs/sections>. Existing code to reuse: <paths>.

## Scope
Files/crates you may create or edit. Everything else is read-only (especially `crates/protocol` unless listed).

## Contract
Public API / behavior other slices rely on (signatures, invariants).

## Acceptance
Observable checks: tests to add, commands that must pass (`just check`), demo steps.

## Parity
Affordances this slice removes or moves (button, panel, shortcut, menu item, setting, layout state), each with its replacement, or "none". UI slices name the screens to check in a real window (`docs/process.md` § UI checks).

## Out of scope
What not to do.

## Report
End with: summary, files changed, commands run with results, open questions. Commit on the slice branch with a conventional message. Do not edit docs/status.md or docs/roadmap.md (the integrator does).
```
