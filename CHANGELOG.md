# Changelog

Newest release first; add new entries at the top, under **Unreleased**. Older releases stay below, unchanged.

## Unreleased

### Editor
- Embedded code editor in file tabs: tree-sitter highlighting, editing with undo, splits, large-file safeguards; files are read and written through the daemon.
- Vim mode (off by default, asked on first run): motions, operators, text objects, Visual/Line/Block, registers, marks, macros, `.`, search with a visible `/` prompt, `:` commands (`:w :q :sp :vs :s :g …`), live mode and position in the status bar, Normal-mode keys work on non-Latin keyboard layouts; behaviour checked against 508 cases recorded from real Vim.
- Vim control keys: ⌃[ is Escape everywhere Vim is on (also on a Russian layout), ⌃C too; Insert ⌃W ⌃U ⌃H ⌃T ⌃D ⌃O ⌃R ⌃A ⌃E ⌃Y; Normal/Visual ⌃D ⌃U ⌃F ⌃B ⌃E ⌃Y scrolling, ⌃A/⌃X numbers (also `g⌃A`), ⌃^, ⌃G, ⌃]/⌃T, ⌃W window commands; ⌃W ⌃H ⌃U ⌃R and history on the `:` and `/` lines. Your own mappings in `editor.vim_mappings` (e.g. `jk = "<Esc>"`, timeout `editor.vim_timeout_ms`), listed on Settings → Keyboard.
- Completion from the language server: popup on trigger characters, while typing or with ⌃Space (⌃N/⌃P in Vim Insert), documentation beside the list, snippet tab stops, auto-imports applied in the same undo step; signature help on `(` and `,`.
- ⌘P opens files by name (`path:line:col` works) with sessions below; Files panel (⌘⇧E) in the left dock; *Open File…* starts in the project folder.
- Project search and replace (⌘⇧F / ⌘⇧H): case, word, regex, include/exclude globs, replace with one undo step per file; files changed since the search are skipped and listed. Session-history search moved to ⌘Y in the Zed keymap.
- Files panel shows git status (M/A/U/R letters, folders with changes marked); the tab strip scrolls to the active tab and caps long titles; the Changes view lists every changed file and its panel row shows the count.
- Language servers run in the daemon, shared by the editor and agents: found on your machine, never downloaded; a folder must be trusted first. Diagnostics with distinct underline styles and gutter shapes, hover (`K`), go to definition (`gd`, F12, ⌘-click) and back (`⌃O`), Problems panel (⌘⇧M). `hutan lsp`, `hutan diagnostics`, MCP `diagnostics`.
- Agent-aware editing: files an agent changes reload in open tabs as one undo step, changed lines are highlighted with per-change Reject (⌘⌥Z), Accept All and Follow; a buffer with unsaved edits gets a conflict bar instead of being overwritten. Terminal paths open in the editor.

### Agents and chat
- Checkpoints: every agent turn that changes files in a git checkout is snapshotted in hidden git refs. *Rewind…* on the turn shows exactly which files go back, flags files edited since, and restores only what you tick; *Undo* reverts the restore, also after a restart. `hutan checkpoint ls|diff|restore|undo|rm`.
- Message queue: typing while the agent works queues the message (editable chips); ⌃↩ sends now (Codex steers the running turn, other agents are interrupted first); Stop clears or keeps the queue.
- Attention that clears: an agent that is working is never shown as needing you; only turns that changed files ask for review; looking at the result quiets it; Finish closes it.
- Finish = commit and push from the Changes view (no PR), with staged/unstaged markers and a message pre-filled from the changed files (`Update app.py and store.rs`) plus one line per agent that made them.
- A session whose finished work you have seen goes back to its own state with a small *Changes to review* link, instead of staying in *Needs you*.
- Clearer approvals (one primary *Allow once*), chat retry, mute per session, the agent's real mode shown.
- Models: display names everywhere (also after a restart, kept offline); pickers mark the current model, order by family and newest first, and tell apart same-named variants.
- Language servers: the Problems panel shows each server's state with Restart for a failed one; the trust bar names every server the folder would start and one answer covers the folder; a rustup `rust-analyzer` without the component is reported as not installed with the install command.

### Workspace and look
- Quiet, consistent visual design: new tokens and primitives, title bar and tabs, sidebar, chat, Agents page, Mawas Home and Settings sheet redesigned; keyboard-only focus ring.
- Two-step first run; Start sheet with pinned primary button; empty states; theme and icon pickers with live preview and `hutan theme` commands.
- **Setup** you can run any time (Hutan menu, Settings → General, palette, `hutan setup`): one flow with your current answers filled in — agents, Vim and the send key, history, background service, appearance; skipping a step keeps its value.
- After a reinstall Hutan offers to run the setup again (optional; projects, sessions and settings stay); after an update it shows *What's new* from this changelog.
- Terminal: link hints, drop and paste of files, resize fixes, command toolbar off the command line.

### Reliability
- When the background service cannot start, Hutan says why and offers the fitting action: data written by a newer Hutan → update command or *Start with empty data…* (old data kept in `backups/`); a stuck service → show or stop that process; anything else → logs and details. `hutan daemon status|why|reset-data`.
- Many flaky tests fixed at their cause; a guard fails the test run if a test leaves a daemon behind.
- File watching no longer misses a write made between reading a file and watching it.

## 0.1.0 — 2026-10-19

First preview build (pre-release): the `hutand` daemon, the desktop app and the `hutan` CLI for macOS on Apple Silicon. Terminal sessions with shell integration, agent chat sessions for Codex, OMP and Claude Code, Mawas the assistant, approvals, worktrees and tasks, history search, themes. Ad-hoc signed, not notarized. Install: `brew install --cask robertt3kuk/hutan/hutan`.
