# TERM-IMAGE-PASTE — paste and drop images into a terminal for agent CLIs

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/TERM-IMAGE-PASTE`

**Contract ownership:** none expected.

## Context
Agent users paste screenshots into Claude Code / Codex / OMP running in a terminal (cmux #3435, #3639, GhD #10517 — `docs/spikes/2026-10-25-user-needs/terminals.md`). Hutan refuses image paste in terminals today (`docs/design.md` § drop and paste). Agent CLIs accept an image **path** typed into the prompt (Claude Code reads `@path`/a dropped path; Codex `--image` and pasted paths; check OMP).

## Items (in commit order)
1. **Paste**: when the clipboard holds an image and no text, ⌘V in a terminal saves it as PNG under the session's scratch dir (`$HUTAN_HOME/scratch/<session>/paste-<n>.png`, pruned with the session, bounded size 20 MiB) and pastes the quoted path (with bracketed paste if the program enabled it); a one-line toast says where it went with *Reveal*.
2. **Drop**: dropping image files already pastes paths (UX-TERM-2) — keep; dropping image *data* (from a browser) saves like paste.
3. **Agent-aware**: when the terminal runs a known agent (integration state), the pasted path is prefixed the way that agent expects (Claude Code: plain path works; document per harness in `docs/integrations.md` after checking each one's docs).
4. Setting `terminal.paste_images` (default on); off restores the refusal.
5. Docs: `docs/design.md`, `docs/integrations.md`.

## Acceptance
GPUI tests with a fake clipboard image: file written, path pasted (bracketed when enabled), size limit refused with a notice, scratch pruned when the session closes; setting off refuses. Cua check: paste a screenshot into a scratch terminal running `cat`-like echo (no real agent needed).

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(TERM-IMAGE-PASTE)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/termimagepaste/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
