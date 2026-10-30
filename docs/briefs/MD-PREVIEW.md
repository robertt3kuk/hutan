# MD-PREVIEW — Markdown preview without a language server

Agent: implementer (Claude Sonnet 5.5)   Branch: `slice/MD-PREVIEW`

**Contract ownership:** none (desktop; no protocol).

## Context
User 2026-10-30 (after updating to 0.3.4 in the app — it worked): "rendering Markdown — why add marksman, maybe there's a crate for it". Today opening a `.md` file shows the LSP trust bar for `marksman` (a Markdown language server) and there is no rendered view; the chat already renders Markdown (check `crates/chat_view` for the parser — pulldown-cmark or similar — and reuse it).

## Items (in commit order; each with a regression test)
1. **Preview**: a Markdown file tab gets *Preview* (⇧⌘V toggles the tab between source and rendered; ⌘K *Markdown: Open Preview to the Side* opens a live preview split that follows edits, scroll-synced by heading/line), rendered with the same Markdown renderer as the chat (headings, lists, tables, code blocks with syntax colour, links that open files/URLs, images from the repo relative to the file, task lists), bounded work per keystroke (debounce, only re-render changed blocks if needed).
2. **No language server needed for Markdown**: `marksman` is not offered/started for Markdown unless the user enabled it in `daemon.toml` (`[lsp.servers.marksman]`); no trust bar for `.md` by default; docs say how to enable it for link checking.
3. Setting `editor.markdown_preview = off | toggle | side` (default `toggle`), and optionally open READMEs in preview mode (`editor.markdown_open = source | preview`, default `source`).
4. Docs `docs/editor.md`, `docs/lsp.md`.

## Acceptance
GPUI tests (toggle, side preview follows edits, links, images, tables, no LSP trust bar for `.md` on a fresh home); unit tests for the renderer bits you add; Cua screenshots on `~/hutan-scratch/e2e6/repos/book` (dark + light); `just check` green.

## Rules
Read `AGENTS.md`, `docs/process.md` (§ Implementer agents, § Definition of Done, § UI checks — note the rule that the user's own Hutan runs on this machine), `docs/spikes/2026-10-25-user-needs/README.md` and the report it cites for this need. Commit after every item (messages end `(MD-PREVIEW)`); tests wait on state; update existing tests to the new spec keeping what they prove; Cua screenshots dark + light into `~/hutan-scratch/mdpreview/shots/`; `just check` green at the end. Fresh `HUTAN_HOME`; no `hutand` left. Do not edit `docs/status.md`, `docs/roadmap.md`, `CHANGELOG.md`.
