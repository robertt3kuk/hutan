# UI-STUDY — compare the best agent-IDE UIs and plan Hutan's UI pass

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/UI-STUDY`

**Contract ownership:** none — research/design spike, docs only.

## Context
User 2026-10-28: "the UI must be normal — compare the UIs of different apps and take the best of them"; terminal agent sessions show almost no info (Orca's cards show agent, model, a one-line summary of the last turn, time); tabs should show the session's name; settings should look nicer (spacing etc., by examples); the quit sheet looks rough; "parse like Orca, which wraps the terminal UI and shows it well and in detail, or like Tern desktop does together with OMP". The user's screenshots are in `~/hutan-scratch/private/` (`hutan-omp-terminal.png`: Hutan 0.3.1 with OMP running in a terminal tab; `orca-omp.png`: Orca with OMP sessions; `quit-dialog.png`: Hutan's quit sheet). They show the user's **private work** — look at them, never copy their text, names or paths anywhere, never commit them (AGENTS.md invariant).

## Deliverable
`docs/spikes/2026-10-28-ui-study/README.md` (+ SVG mocks, our own, no screenshots of the user's data) answering:
1. **How Orca and Tern understand terminal agents**: read Orca's source (partial clone `~/proj/research/stablyai_orca`, or clone `stablyai/orca` again into `~/proj/research/`) — how it detects an agent in a terminal, where tab titles, agent/model, last-turn summary and state come from (OSC 0/2 titles, OMP's **OSC 7501** `ESC]7501;state=working|blocked|done|error|idle|clear[:kind=…]:app=<name>[:msg=<base64>]ESC\`, hooks, session JSONL files, OMP RPC?). Tern: OMP's own terminal (`omp` settings mention Tern panes; find its docs/source) — what it renders natively for OMP. Cite file paths/links.
2. **UI comparison** (screens, measurements, patterns) across Orca, Tern, Zed, Cursor (incl. Agents window), Warp, Conductor, Superset, VS Code, Linear/Raycast-style settings: session cards in a sidebar, tab titles and icons for agents, state indicators, settings layout (navigation, section headers, row spacing, descriptions, controls alignment, search), confirmation dialogs (quit with running work), dock toggles. Use public docs, screenshots from their sites/repos and source where available.
3. **Hutan today** (run master on a scratch `HUTAN_HOME` with Cua: sidebar cards, tabs, settings sheet, quit sheet, docks) vs the best patterns: a concrete gap list with measurements (paddings, font sizes, row heights).
4. **Plan**: slices in commit order with mocks: terminal-agent cards and tabs (what data, from where), settings redesign (spacing scale, layout), dialogs (quit), anything else top-impact. Protocol impact per slice.
Rules: docs only; don't build the workspace (other agents gate) — use an existing debug binary from another agent's worktree under `../hutan-wt/*/target/debug` if present, else `cargo build -p hutan-desktop -p hutan-daemon` once; scratch `HUTAN_HOME`; never touch the user's own Hutan; never pkill by name; Cua: switch input source to ABC, restore Russian after.

## Rules
- Work only in `../hutan-wt/UI-STUDY`; prototypes/screens under `~/hutan-scratch/uistudy/`; do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`. Finish: commit the docs on `slice/UI-STUDY`, report in ≤15 lines.
