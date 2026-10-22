# FX-E2E2-B — model names everywhere, model picker clarity, LSP states that tell the truth

Agent: implementer-opus (Claude Opus 5.5)   Depends on: E-12a, E-11b merged   Branch: `slice/FX-E2E2-B`

**Contract ownership:** none expected (if the daemon needs to persist a model's display name with the chat, that is a daemon/storage change, allowed; a wire field needs a stop-and-report).

## Context
Second e2e pass `docs/spikes/2026-10-23-e2e-usertest-2.md` § Models, findings 2–3 (shots in `~/hutan-scratch/e2e2/shots/`). The user asked that Hutan shows **the correct models from OMP**: the list itself is correct (29 models, matches `omp models`, Opus 5.5 default); what is wrong is how names show.

## Items
1. **Display names, always** (shots 70 → 74, 76): after an app restart the chat header shows the raw `claude-haiku-5-5`; the Agents page row shows `anthropic/claude-hai…`. The display name must survive restarts and appear wherever a model is shown (chat header/status line, Agents page/rail rows, Today, task rows, paid-call confirm, Mawas). Source: the harness catalog (`chat.models`, cached by the daemon); persist the chosen model's display name with the chat (daemon side) or resolve through a desktop catalog cache that loads at startup per harness in use — choose the one that works offline after a restart, without extra harness spawns per row. Never show a truncated raw id when a name exists; tooltip carries the id.
2. **Picker clarity** (shots 57–61, 65, 66): mark the **current** model (check mark + selected row) in the Start sheet menu, Switch model sheet and Mawas Switch model; keep the *default* tag. Duplicate display names (`Claude Haiku 4.5` for `claude-haiku-4-5` and `claude-haiku-4-5-20251001`, same for Opus/Sonnet 4.5) are disambiguated by showing the dated variant as `Claude Haiku 4.5 (20251001)` in muted text — the catalog's names are untouched. Order within a provider: by family (Opus, Sonnet, Haiku, others), newest version first, aliases before dated ids; the default first in its family. The filter box matches names and ids.
3. **Problems panel truth** (shots 41–43): the footer says "Some language servers are still starting" while rust-analyzer is `failed`. Show per-server state lines: `Rust · failed: <reason> · Restart`, `Plain Text · ready`; the "still starting" line only while one is starting/indexing.
4. **Trust wording** (shots 11, 36): trust is per folder and starts every configured server there; the bar must say so: "Language servers for Rust and Plain Text run code from this folder (build scripts, tools). Trust ~/…/acme-api?" listing every server that would start; *Not now* on one bar applies to the folder (no second bar for the next language).
5. **rustup proxy without the component** (finding 2's cause): `rust-analyzer` on PATH is rustup's proxy; without the component it fails ("infinite recursion detected" in the tester's env, or "Unknown binary 'rust-analyzer'"). Detect this at discovery (proxy present, `rustup which rust-analyzer` fails or the binary's first run exits with rustup's message) and report `not_installed` with the command `rustup component add rust-analyzer`, never a crash loop.

## Acceptance
GPUI tests: header and Agents row show display names after an app restart with no harness spawn in between (count spawns); current model marked in all three pickers; duplicate names disambiguated; order as specified (unit test on the sort); Problems panel lines per server state; trust bar names all servers and one decision covers the folder. Daemon test: a fake rustup proxy on PATH yields `not_installed` with the command. Cua screenshots (real OMP harness on Haiku is allowed for the header shot — `anthropic/claude-haiku-5-5` only): picker with current mark, header after restart, Problems panel with a failed server → `~/hutan-scratch/fxe2e2b/shots/`. `just check` green; commit per item, messages end `(FX-E2E2-B)`. Fresh `HUTAN_HOME`; no process left.

## Out of scope
Everything else in the report; `docs/status.md`, `docs/roadmap.md`.
