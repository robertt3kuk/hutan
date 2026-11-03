#!/bin/sh
# A third-party agent telling Hutan about itself: the whole "Report your agent's stats to Hutan"
# contract at level L2 (docs/integrations.md), in a few lines of shell. It plays one turn of an
# imaginary agent called `acme`: it says it is there, starts a turn, reports two model calls, the
# tools and files of the turn, and ends it. Each insight is one of the JSON files next to this
# script (`insight-reports/`, valid against docs/schemas/agent-insight-report-v1.json).
#
# Run it inside a Hutan terminal: `hutan agent report` does nothing (and exits 0) anywhere else, so
# an agent can call it unconditionally. `HUTAN` names another `hutan` binary (the tests use it).
#
#     integrations/examples/third-party-agent.sh && hutan chat insight "$HUTAN_SESSION_ID"

HUTAN=${HUTAN:-hutan}
here=$(cd "$(dirname "$0")" && pwd)

# --source names who reports (a sequence number is kept per source), --agent the agent.
report() { "$HUTAN" agent report --source acme-agent --agent acme "$@"; }
# An insight from a file here, over standard input (`--insight-json -`): insight FILE [report options].
insight() { file=$1; shift; report "$@" --insight-json - < "$here/insight-reports/$file.json"; }

report --state idle                          # the agent is there (always the first report)
insight 01-turn-start --state working        # a turn begins
insight 02-model-call                        # tokens, context, cost of the first model call
insight 03-tools-and-files                   # what the tools did meanwhile
insight 04-second-call                       # the second call (the cost is cumulative)
insight 05-turn-end --state idle             # the turn ends
