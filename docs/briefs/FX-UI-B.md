# FX-UI-B — chat header, terminal chrome, fork popover, settings polish

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/FX-UI-B`

**Contract ownership:** none (desktop, terminal; no protocol).

## Context
Real-window evaluation `docs/spikes/2026-10-29-ui-wave-test.md` (screenshots `~/hutan-scratch/e2e8/shots/` by number).

## Items (in commit order; each with a regression test)
1. **P2-2 / fix 2** the folders chip (and the Contain chip) never sits over terminal output: move it to the tab/title area or a reserved strip outside the cell grid; the terminal's rows/cols must not shrink silently (resize the PTY if a strip is added).
2. **P2-4 / fix 3** the chat header always shows the **model**: lineage and folder chips shrink to icon-only first (text in tooltip), the rest folds into ⋯; never clipped at the right.
3. **P2-5** *Fork Session* for a terminal agent without a known session id says what supplies it ("Install OMP hooks in Agent integrations…" with the link); for OMP also try the session id from OSC 7501/title if OMP exposes it (check OMP's `/session` or env; no `~/.omp` reads).
4. **P3-13** investigate stale `✓ 0 ms` status lines left at wrong rows of OMP's transcript after it scrolls inside Hutan's terminal (OMP draws correctly elsewhere): record the bytes in a scratch run (`script`/the corpus recorder), find the terminal-engine cause (scroll regions / DECSTBM / partial redraw / synchronized output), fix with a corpus test.
5. **P3-14** light terminal + OMP: answer OSC 10/11 colour queries with the active terminal palette (if not already) so TUIs adapt; else offer `terminal.theme_follows_app = false` (terminal stays dark in light mode) and document it.
6. **P3-15 / fix 10** divider hover is a 1–2 px accent line; **P3-16** usage ring 16 px with better contrast; **P3-17** row context menus get separators and short disabled reasons (detail in tooltip); **P3-18** Settings `@modified`/`@restart` chips toggle off and combine as OR, page description not cut; **P3-21** fork popover: informational limit in neutral tone, radios aligned to text baseline, cut choice first; **P3-22** Start sheet default model is the harness's default but never Opus/Sonnet for test homes — keep the user's own default, fix the 6 px layout jump; **P4** quit-sheet sentence simpler, palette ranking ("fork" shouldn't match *Add Folder…* above *Fork*), Context panel *Files touched* shown.
7. Docs `docs/design.md`.

## Acceptance
GPUI tests per item, a terminal corpus test for item 4; Cua before/after screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(FX-UI-B)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/fxuib/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
