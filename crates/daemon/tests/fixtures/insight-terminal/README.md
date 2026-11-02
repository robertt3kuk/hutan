# Recorded reports of the OMP extension (INSIGHT-B)

`omp-extension-reports.jsonl` is what `integrations/omp/hutan-agent-state.ts` (the extension
`hutan integration install omp` installs) sent in a **real OMP 18.14.1 run**: one JSON array per
line, the argument list of each `hutan agent report` the extension spawned, in order. `HUTAN_BIN`
pointed at a script that only appended its arguments to a file, so no daemon took part.

How it was made (all in a scratch `HOME` / `PI_CODING_AGENT_DIR`, nothing of `~/.omp` read, no login
and no network but loopback): OMP's interactive TUI in a pty with `-e <extension> --model
mock/mock-1`, a local OpenAI-compatible provider (`mock-openai-tools.py`, cost table 3 / 15 /
0.3 per Mtok in `models.yml`) that answers the first prompt with a `read` and a `write` tool call and
then "Done.", and a second prompt that is answered directly. The numbers (8 300 input, 14 000
cache read, cost 0.03012 USD for the first turn) are the mock's, not a model's.

The recording shows the first-run welcome and setup screens were skipped by the driver; nothing else
was scripted. The extension's `session_compact` and `model_select` handlers were **not** exercised
(no compaction or model change happened).
