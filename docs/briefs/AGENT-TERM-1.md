# AGENT-TERM-1 — terminal agents: state, name and icon from OSC 7501 and titles

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/AGENT-TERM-1`

**Contract ownership:** **delegated**: additive protocol minor **0.55** (capability `terminal.agent`) if new session fields are needed (`SessionInfo.agent {app, state, kind?, message?, since}`), own commit, goldens.

## Context
User 2026-10-28: "when an agent starts in a terminal, the tab at the top and the row on the left must show it is no longer a plain terminal, and the session name must load correctly (OMP renames its session, Hutan still shows the program name; I renamed the tab by hand)"; "parse like Orca". The user's screenshots are in `~/hutan-scratch/private/` (`hutan-omp-terminal.png`: Hutan 0.3.1 with OMP running in a terminal tab; `orca-omp.png`: Orca with OMP sessions; `quit-dialog.png`: Hutan's quit sheet). They show the user's **private work** — look at them, never copy their text, names or paths anywhere, never commit them (AGENTS.md invariant).

OMP 18.12 emits **OSC 7501** program status (read from its binary): `ESC ] 7501 ; state=<working|blocked|done|error|idle> [:kind=<kind> when blocked] :app=<name> [:msg=<base64 of a ≤2048-char text> when blocked or error] ESC \`, and `ESC]7501;state=clear ESC\` on exit; it also sets the window title (OSC 0/2) to its session title (e.g. an emoji + a short task name). Hutan already keeps `osc_title` (`crates/daemon/src/registry.rs`, naming in `session_names.rs`) and has agent reporting through installed hooks (`docs/integrations.md`).

## Items (in commit order)
1. **Terminal sniffer** (`crates/terminal`, beside OSC 133/7): parse OSC 7501 (bounded, base64 decoded, redacted like other agent text), unit-tested on the exact OMP format incl. `clear`; record a corpus line from a real `omp` start (no model call needed for `idle`; one Haiku turn via `omp --model anthropic/claude-haiku-5-5` in a scratch dir for `working`/`done` if needed).
2. **Daemon**: per terminal session `agent {app, state, kind, message, since}`; it feeds the existing attention model like integration reports do (blocked → Requests with the question text; error → Failed; done after working with file changes → Ready for review), without double-counting when hooks also report; `state=clear` / process exit clears it. Agent detection also from the foreground process (known harness binaries) so the icon shows before the first OSC.
3. **Naming**: a session with an agent is named from the agent's OSC title when the user hasn't locked the name; a user-locked name stays but the agent title shows as a subtitle; `session_names.rs` rules + tests.
4. **Desktop**: tab and sidebar row show the harness icon (OMP/Claude/Codex glyphs from the icon pack) instead of the terminal glyph and a state dot (working spinner / blocked / error / done), tab title = session name; the sidebar row shows a second line with the blocked question or error message (one line, muted).
5. CLI `hutan ls` shows agent + state; docs `docs/integrations.md` (OSC 7501 section), `docs/design.md`, `docs/protocol.md`.
(Richer cards — model, last-turn summary — wait for UI-STUDY's findings.)

## Acceptance
Terminal unit tests; daemon tests feeding recorded OSC 7501 bytes through a PTY (`printf`), attention transitions, naming with/without a locked name; GPUI tests for icon, dot, title, second line; Cua screenshots with a real `omp` in a scratch terminal (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(AGENT-TERM-1)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/agentterm1/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
