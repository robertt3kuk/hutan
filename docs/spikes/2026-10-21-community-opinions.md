# Community opinions (HN, Reddit, issues) — and what they change in Hutan's decisions

Date: 2026-10-21 · Method: six `researcher-haiku` analysts, one topic each; raw evidence with links, scores and search logs in `docs/spikes/2026-10-21-opinions/` (`orchestrators.md`, `delivery.md`, `editor.md`, `assistant.md`, `onboarding-ui.md`, `approvals-modes.md`). Synthesis and decisions by the orchestrator (Opus).

**Limits.** Hacker News was read through the Algolia API (scores and comments counted by hand; agree/disagree counts are the analysts' reading). **Reddit blocked the analysts (403 / bot challenge)**: Reddit evidence is search-snippet level, without scores. GitHub issue reactions are exact. Treat counts as directional, not statistics.

## Verdicts

| Decision (2026-10-21) | Community evidence | What Hutan does |
|---|---|---|
| One "needs you" queue across agents is the core | **Supports**: ~11 voices ask for it, ~3 say hooks/notifications suffice; Anthropic's own Agent View is a direct threat to a single-vendor queue (`orchestrators.md`) | Keep as the headline; the moat is **cross-agent** (Claude, Codex, OMP) queue **plus** review and finish, not the list alone |
| Native, light Mac app | **Mixed**: ~8 voices for (RAM, speed), ~2 defend Electron | Say it with measured numbers (RAM per agent, idle CPU), not as a tagline |
| Terminal-faithful agents, not chat-only | **Mixed**: ~7 for terminal fidelity, ~11 for a GUI multiplexer around real terminals; chat-only panels get the most pushback | Unchanged: real PTYs with a GUI layer, chat as an optional view |
| Finish = commit and push to main, no PR (user) | **Mixed, against agent-driven pushes**: trunk-based humans support it (~3–4); the strongest objection (~5–6) is an *agent* committing/pushing to main without review, "the tool call itself needs to be gated" (`delivery.md`) | **Finish is a user action**: one button commits (main checkout or merge of a worktree) and pushes the current branch to its upstream. Agents can never push: `git push` from an agent session is not something Hutan does for them, and the daemon's finish request is `UserOnly`. No PR step for now. ADR 0020 |
| Default placement: main checkout, worktree suggested | **Leans supports**: ~6 for offering worktrees without forcing; setup cost is the repeated complaint | As decided; add a setup hook for worktrees later (`.worktreeinclude`-like) |
| Checkpoints off by default (user) | **Contradicts**: Claude Code, Cursor and Copilot CLI ship them **on**; Codex rewind request +229; default-off support ~2 voices on another feature | **Changed to on by default** for agent chats in git checkouts, with ADR 0019's hard caps and an easy per-project/global off; honest "files only, Bash edits may be missed" label (as Claude's docs do). The user asked to follow the research; this overrides the earlier "off" answer and is a one-line setting to flip back |
| Mawas as a headline feature (user) | **Leans against a persona headline**: assistant threads get attention but their top replies are security/privacy; default-on memory/recording drew the sharpest backlash (Recall, Screenpipe, Cursor iOS); the validated job is "tell me when an agent needs me" | Mawas stays a headline page (user decision), but **leads with the needs-you job** (Today first), memory stays curated and user-approved (MW-2), nothing observes or records by default, memory is inspectable/editable/deletable/exportable |
| Built-in editor with first-class Vim | **Mixed**: 3 for, 4 against, ~15 keep nvim beside the agent | Keep M11, positioned as a **secondary surface** next to agents (open what the agent touched, fix, go back); always offer *Open in external editor*; Vim fidelity high (and later honour a Vim config subset) |
| Vim default off + first-run question | No direct evidence | As decided |
| LSP never auto-downloaded | **Supports**: Zed #12589 309👍/1👎, HN 40902826 (~12 vs ~4) | As decided; prefer installed servers, ask before any download (none in v1) |
| 2-step first run, just-in-time asks | **Mixed**: per-action prompts are disliked (93–97 % approval rate per Anthropic), but explicit consent before background behaviour is wanted | 2 steps; **batch** just-in-time asks (once per project, not per action); never start background behaviour without consent |
| Clean default layout, progressive disclosure | **Mixed**: the backlash is against **forced** layout changes (Zed 1.0 "agent-first layout I cannot undo"), not minimalism | New installs get the clean default; existing users get an offer, never a forced migration; a one-step way back (*Layout: Classic*) |
| Local-first, no account | **Supports**: ~10 voices against login walls (Warp reversed its login requirement) | Keep; state it on the first screen |
| Queue (not steer) while the agent works | **Leans queue as default**: Claude Code #50246 (254 reactions); steering demand is real (6 items) | Queue by default; *Send now* (steer at the next tool boundary) as a labelled second action where the harness supports it |
| Show the real mode, never change it silently | **Supports** never-silent change (mode resets and auto defaults broke trust) | As decided (UX-MODE-CHIP) |
| Safe-default approval cards with "always" scope | **Supports** (6 for, 1 against) | Done (UX-APPROVAL-CARD); next: batch approvals per plan, narrow reversible rules |

## Takeaways that add work

1. **Approval fatigue:** ask once per plan or scope, not per action (Anthropic: plan approvals rejected 39 % vs 3 % per prompt). → candidate slice `UX-APPROVE-PLAN` after UX-QUEUE-STEER.
2. **Notify on "needs you" with focus-aware suppression and a quiet mode** (Codex #3962 +222 vs nagging complaints 348–984 pts). → UX-PROTO-1 card 1 already covers mute/focus; add a quiet-hours preset.
3. **Intent/conflict between parallel agents** in one repo is the next pain after worktrees (HN 49789356, 44533339). → research item after M11.
4. **Measure and publish RAM per agent and idle CPU** (native claim). → `docs/perf.md` public table, README line.
