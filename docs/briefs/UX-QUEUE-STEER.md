# UX-QUEUE-STEER — queue while the agent works; Send now steers; Stop stops

Agent: implementer-opus (Claude Opus 5.5)   Depends on: PR-FINISH merged (protocol 0.35)   Branch: `slice/UX-QUEUE-STEER`

**Contract ownership:** delegated: additive minor **0.35 → 0.36**, capability `chat.queue`.

## Context
Read `AGENTS.md`, `docs/spikes/2026-10-21-ux-plan.md` card #22 and decision D1 (user, 2026-10-21: the send key **queues** while the agent works), `docs/spikes/2026-10-21-community-opinions.md` (queue default; steer as a labelled second action at the next tool boundary; cancel-to-steer loses work), today's code: `crates/daemon/src/chat.rs` (refuses a second send), `wb/chat/view.rs`, `wb/chat/model.rs`, SK-1 send-key rule (`docs/design.md` § Send key).

## Scope
Daemon-owned queue per chat session (bounded, persisted with the journal): `chat.queue.add|edit|remove|clear`, events; after a turn ends the next queued message is sent automatically; **Send now** sends at the next tool boundary where the adapter supports steering (ACP/Codex: check what each supports; otherwise interrupt-then-send with an explicit label "interrupts the turn"); **Stop** interrupts and clears the queue unless the user chooses *Keep queue*. Desktop: queued messages as editable chips above the composer, the send key queues while a turn runs, a separate chord for Send now, the Stop menu. CLI `hutan chat send --queue|--now`.

## Acceptance
Tests: send during a turn queues and is sent after the turn ends (fixture harness); editing/removing a chip changes what is sent; Stop clears the queue; Send now on a harness without steering interrupts then sends and the UI said so; queue survives a desktop reconnect and a daemon restart; bounded (e.g. 20 per session).

## Acceptance (common)
Screenshots with Cua Driver per `docs/process.md` § UI checks (before the final gate); `just check` green (machine-wide lock); commit after each logical step; messages end `(UX-QUEUE-STEER)`. Fresh `HUTAN_HOME` for every run; no `hutand` left.

## Parity
List every moved or removed affordance with its replacement (Definition of Done).

## Out of scope
`docs/status.md`, `docs/roadmap.md`; anything not named here.
