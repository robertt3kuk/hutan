# FX-BLOCK-REFLOW — block markers drift after the terminal reflows

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-BLOCK-REFLOW`

**Contract ownership:** none (bug fix; no protocol change expected — report first if one is needed).

## Context
User 2026-11-02 (0.3.5, while recording a demo): after the terminal reflowed because the right panel opened (narrower width), the block markers moved to the wrong rows. Observed screen (user's screenshot, kept outside the repo): a shell with `cd … && clear` (✓), `git status -sb` (✓), `just --list | head -8` whose long lines wrap at the new width, then `git switch release/0.4` which failed with exit 128, then `ls crates | wc -l`. After the reflow, the red `✗ 128 32 ms` marker and the red block edge sat on the `deny` / `fmt` lines inside the `just --list` output, and `git switch`'s row carried `✓ 11 ms` (which belongs to the next command) — every marker after the wrapped output shifted up by the rows the wrap added, i.e. markers keep stale row positions while the text is reflowed.

## Change
Reproduce in a test first (terminal crate corpus or a GPUI test: OSC 133 blocks, long lines, width shrink → reflow), find where block positions (OSC 133 prompt/command/end marks, the block index, the gutter/edge and the status chip) are stored as absolute rows and not remapped through the reflow, and fix at the root so every block keeps its own lines across any width change (shrink and grow, scrollback included, blank-row trimming from UX-TERM-1 included). Keep the B4a/B4b reflow budgets (`docs/perf.md`); report before/after.

## Acceptance
Regression test that fails before and passes after (several blocks, wrapped output, shrink then grow, a failing command keeps its ✗ and exit code on its own row); Cua screenshot of the same sequence in a real window after opening the right panel (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-BLOCK-REFLOW)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxblockreflow/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
