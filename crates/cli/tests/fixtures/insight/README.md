# Insight fixtures of the terminal agents' integrations (INSIGHT-B)

Hand-built from the **documented** shapes, not recorded: no login of Claude Code or Codex was
used (scratch homes only, nothing under `~/.claude` or `~/.codex` was read), so these payloads are
**unobserved** in a real session. They carry placeholder values only; none is user data.

- `claude-posttooluse-edit.json`, `claude-posttooluse-write-create.json`, `claude-posttooluse-failure.json`:
  Claude Code hook input of `PostToolUse` / `PostToolUseFailure` (<https://code.claude.com/docs/en/hooks>),
  including the `duration_ms` field.
- `codex-posttooluse-apply-patch.json`: Codex hook input of `PostToolUse` for `apply_patch`
  (<https://learn.chatgpt.com/docs/hooks>); where the patch text sits in `tool_input` is an assumption (`command`).
- `claude-statusline.json`, `claude-statusline-first-request.json`: the JSON Claude Code writes to the
  `statusLine` command (<https://code.claude.com/docs/en/statusline>), after a request and before the first one.

What was recorded from a real run is the OMP extension's reports: `crates/daemon/tests/fixtures/insight-terminal/`.
