# SPIKE-ADVISOR-NET — can an unattended Advisor turn reach the network?

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/SPIKE-ADVISOR-NET`

**Contract ownership:** none — a spike: report only (`docs/spikes/2026-10-31-advisor-network.md`), no product code.

## Context
ADR 0025 D4 and spike `docs/spikes/2026-10-30-mawas-supervisor.md` §2.8 step 3: a scheduled **model digest** may run only with the Advisor profile, and only after we confirm, per harness, that its Advisor launch has **no outbound network tool** — agent transcripts are in the context and a hostile one is untrusted input (prompt injection → exfiltration). This spike gathers that evidence; MAWAS-DIGEST-B is built only if it says yes (or says exactly which launch flags make it yes).

## Questions
1. **OMP** (installed): exactly what Hutan launches for Advisor (read `crates/daemon` assistant backends and `docs/control.md` § Mawas). With that launch: which tools exist (built-in and MCP)? Does it load the user's own MCP servers, extensions, skills, hooks or plugins from the user's config — and can a per-launch flag/overlay/env (like MAWAS-READS' `--config` overlay) turn all of those off? Can any remaining tool fetch a URL (web search/fetch, bash, `read` of a URL, browser, LSP, python/eval)? Prove with real runs on `anthropic/claude-haiku-5-5` in a scratch home: ask the Advisor-launched OMP to fetch a canary URL you control locally (a `python3 -m http.server` on 127.0.0.1 logging requests — note that localhost is still network) and to list its tools; record whether any request arrives.
2. **Claude Code** and **Codex** (not installed): from their official docs/sources only — what the Advisor launch (`plan` mode; Codex read-only sandbox) allows network-wise (WebFetch/WebSearch in plan mode? Codex sandbox network off?), user MCP servers loaded, and the flags that disable them. Mark everything unobserved.
3. The Hutan MCP server itself in Advisor: confirm only read tools are reachable and none of them fetches the network.
4. Recommendation per harness: safe as launched / safe with these exact flags / not safe; and what MAWAS-DIGEST-B must enforce (refuse to schedule for a harness that isn't proven).

## Rules
Never read `~/.omp`, `~/.claude`, `~/.codex` contents (learn OMP's config surface from `omp --help`, its docs, the binary's strings, or a scratch `HOME`/config dir); real model turns only through OMP on `anthropic/claude-haiku-5-5`, ≤ 6, scratch dirs; never touch the user's own Hutan; never pkill by name. Commit the report on the slice branch; no `just check` needed for a docs-only change (run `just check` only if you touch code — you shouldn't).
