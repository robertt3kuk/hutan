# Transcript fixtures for insight (INSIGHT-C)

`claude-session.jsonl` and `codex-rollout.jsonl` are **synthetic** transcripts (nothing of a real session; paths, ids and numbers are invented) in the shapes `Adapter::usage_line` reads. They are used by `crates/index/tests/it/usage.rs` and by the daemon's `tests/it/insight_transcript.rs` (which rewrites `/work/app` and the conversation id to a temporary folder).

Which lines are of a shape that was **recorded** from a real run and which are built from what the agents' source and documentation say:

| Line | Shape | Status |
|---|---|---|
| Claude `system` with `contextUsage {model, total_tokens, raw_max_tokens, categories[{name, tokens, kind}]}` | the `/context` record | **recorded** (Claude Code 2.1.312, `-p /context`, scratch home; numbers are the ones of `docs/spikes/2026-10-28-insight/claude-context-usage.json`). Whether an interactive `/context` writes it as `-p` does is not known |
| Claude `cost-state` | `totalCostUSD`, `totalAPIDuration`, … | keys **recorded** (zeros, nothing was called); their nesting under `costState` here is a guess, the reader accepts them at the top level too |
| Claude `assistant` with `message.id`, `message.model`, `message.usage {input_tokens, cache_creation_input_tokens, cache_read_input_tokens, output_tokens}`, `stop_reason`, `isSidechain` | per-message usage | keys **reported** by the AGENT-TELEMETRY spike (2.1.316, local mock); **not recorded** here |
| Claude: one line per content block with the message's usage repeated and growing | the split message | **not recorded** (from public knowledge of the transcript format) |
| Claude `user` with `tool_result {tool_use_id, is_error}` and `toolUseResult {durationMs, type: "create"}` | tool end | **not recorded** |
| Claude `user` with `[Request interrupted by user]` | interrupted turn | **not recorded** (the history index already treats it as the harness's own notice) |
| Codex `session_meta`, `turn_context`, `response_item` messages | identity, model, messages | the shapes of the history adapter's own recorded tests |
| Codex `event_msg` `token_count {info {total_token_usage, last_token_usage, model_context_window}}` | usage | **reported** by the spike (Codex 0.166, window 258 400); fixture numbers invented |
| Codex `exec_command_end {exit_code, duration {secs, nanos}}`, `patch_apply_end {success, changes}`, `mcp_tool_call_end {invocation, duration, result}`, `task_started`, `task_complete`, `turn_aborted` | tools, files, turn edges | **not recorded** (from Codex's public protocol source) |
| Codex top-level `token_usage_record` (0.166) | repeats `token_count` | seen by the spike; not in the fixture; the reader ignores it |

A shape that turns out different in a real session is a fixture and a reader to correct, not a reason to read more text: the readers keep numbers, names, paths and times only.
