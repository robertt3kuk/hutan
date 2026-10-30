# TABS-FILES — tabs that close their session, one tab per file, files beside the agent

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/TABS-FILES`

**Contract ownership:** none (desktop; no protocol).

## Context
User 2026-10-30 (after updating to 0.3.4 in the app — it worked): "when you close the tab at the top it should close on the left too"; "why can't we open several files in several tabs"; "the possibility to open [a file] right next to the agent".

Today: `terminal.tab_close = detach | close_session` (default `detach`) — closing a tab leaves the session in the sidebar; FX-WORK-2 made a single click in Files open an italic **preview tab** the next click replaces (`editor.preview_tabs = true`); *Open to the Side* exists only in the Files row menu (EX-4).

## Items (in commit order; each with a regression test)
1. **Closing a session's tab closes the session** by default for every session kind (terminals, terminal agents, chats): rename/generalise the setting to `tabs.close_session = always | when_idle | never` (default `when_idle`: closes the session when nothing runs in it; when something runs, ask "… Closing stops it." with *Keep running in background* / *Close*), the existing *Closed … · Undo* toast restores; `terminal.tab_close` keeps working as an alias (parity rule); splits follow the same rule.
2. **One tab per file by default**: `editor.preview_tabs` default **false** (every file opened gets its own tab; the preview behaviour stays as an option and the rows still don't shift during ⇧/⌘ selection).
3. **Beside the agent**: when the focused center item is an agent (chat or terminal agent) and a file is opened (Files click, ⌘P, a link in the chat, *Show in Files* → open), it opens in a split to the right of the agent instead of replacing it (`editor.open_beside_agent = true`, reusing an existing right split if there is one); ⌥-click / ⌥↩ always opens to the side; ⌘K *Open to the Side*.
4. Docs `docs/design.md`, `docs/editor.md`.

## Acceptance
GPUI tests per item (close idle/running/agent/chat sessions, Undo, alias; several files → several tabs; open beside a chat and a terminal agent, reuse of the split, ⌥-click); Cua screenshots (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(TABS-FILES)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/tabsfiles/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
