# Changelog

Newest release first; add new entries at the top, under **Unreleased**. Older releases stay below, unchanged.

## Unreleased

### Added
- Find in a terminal: ⌘F in a terminal searches its whole scrollback in the daemon (wrapped lines joined, case and regex options, optionally only the selected block's output), ⌘G / ⌘⇧G step, matches highlighted; `hutan terminal search`.
- Instruction files: an **Instructions** panel lists the files each agent reads (AGENTS.md, CLAUDE.md, OMP's rules; project and user level) and which agent reads which; *Share one file with every agent…* makes AGENTS.md the one file (CLAUDE.md imports it) after showing exactly what it writes, with Undo; Mawas knows how the project's AGENTS.md begins.
- Mawas's folder: Files, Search and Problems show `~/Hutan` at Home and follow Mawas's scope while Mawas Home is in front; Mawas's ⋯ says where she works with *Show Folder in Files*, *Open in Finder*, *Open Terminal Here*; *Track changes in this folder…* turns the folder into a git repository (after a confirm, nothing leaves the machine) so Changes, checkpoints and *Rewind…* work for what Mawas writes; `hutan project git-init`. Finish without a remote commits only.
- Usage and limits: the Agents page has a **Usage** section — each harness's limit windows with reset times (Codex and Claude Code as they report them, OMP via `omp usage`), tokens today and this week, costs marked as estimates; an optional status item (`Codex 72% · resets 3h`); a warning at 80 % and 95 % of a limit, once per window; `hutan usage`, MCP `usage_summary`.
- Mawas memory as files: the memory is mirrored as Markdown in `~/Hutan/.mawas/memory/` (and, if you turn it on, `<project>/.hutan/memory/`); editing a file proposes the change for your approval, never applies it silently; `hutan memory export|import|files` and Export/Import in the Memory panel; in a tracked folder the files are versioned with git like anything else.
- Git history and blame: *Toggle Blame* shows author · date · message per hunk in the editor gutter (unsaved lines show as uncommitted; optional inline blame for the caret line), *Show File History* lists a file's commits across renames with their diffs and *Open at this revision*, and Changes has a **History** segment for the checkout's recent commits; `hutan git log|blame|show`, MCP `git_log`, `git_blame`.

### Changed
- Terminal splits like Ghostty and iTerm: ⌘D / ⌘⇧D split the terminal inside its tab (new terminal in the same folder), ⌘⌥ arrows move focus, ⌃⌘ arrows resize, ⌘⇧↩ zooms, ⌘W closes a split, ⌘⇧W the tab; drag dividers, double-click to even out, right-click menu, drop a terminal tab on another's edge to merge; unfocused splits dim slightly.
- The sidebar follows where you work, like Orca: only the active project is expanded with its sessions and worktrees (others collapsed with request/working counts), switching projects swaps the center tabs, Files/Changes/Search/Problems/Agents show which project they show, ⌃⌥↑/↓ cycles projects, folders and repos look different; `sidebar.show_all_sessions` brings the full tree back. **Files** is now in the right sidebar.
- ⌘K opens the command palette (outside terminals) and the palette ranks names that start with what you type, then commands with keys, first; the Zed base's ⌘K chords are gone (focus ⌘⌥ arrows, hover ⌘⌥I). Start sheet model list is a popover scrolled to the model in use; the Changes view has one segmented filter; the chat header, question card, Requests buttons and Settings rows are calmer; panels no longer repeat their title.
- Every widget is yours to arrange: each panel's ⋯ (or a right-click on its rail icon, nav row or dock tab) moves it Left / Right / Bottom / to a center tab, hides it, or opens its own settings in place; right-click the rail, a dock or the status bar for a show/hide checklist, drag to reorder; sidebar sections can be hidden and reordered. Changes go into a *Custom* preset (built-ins stay untouched) with a 10 s *Undo* toast.
- Attention has three groups: **Requests** (an agent waits for your approval, answer or plan — the only thing the status bar counts), **Failed**, and **Ready for review** (a turn that changed files, until you look). A turn that changed nothing only marks its row unread. "Needs you" is gone.
- Smooth motion: toasts, popovers, menus, pickers, the palette and sheets fade and slide in and out (100–200 ms, ease-out, exits faster; a keyboard close is instant and the keyboard comes back at once), queue chips and answered approvals fold away, Settings' Advanced folds; nothing on the typing path moves. Reduce Motion is followed live (or set `appearance.reduce_motion`): no movement, only short fades.
- How Hutan calls you: the Dock badge counts Requests only (`99+`); a request for a session you can't see shows a toast (≤ 2, 5 s, hover pauses, ⌘⇧R reviews) and a dot on its tab until you look; sound only for Requests and failures; macOS notifications for finished work at most once per session per 90 s (`notify.finished`); muted sessions stay silent.
- One button system everywhere: primary / secondary / ghost / destructive / link and icon buttons in two sizes with hover and press fades, keyboard-only focus ring, disabled and loading states; one primary per surface, Cancel before the primary, Return confirms and Esc cancels — destructive confirms are never the default; sentence-case, verb-first labels.
- The status bar says something again: the background service's state always (`● hutand`, `○ Stopped · Start`, `▲ Can't start · Details` — click for the daemon popover), the project and branch you work in, `◌ N working` while agents run, and the requests that wait.
- Visual polish: one selection fill per list, light-mode cards with a hairline, short chat titles (≤ 40 characters, the prompt in the tooltip), the title bar names the project instead of repeating the tab, toasts bottom-right of the center with a link-style Undo, inset selection bars, neutral pills, one primary per surface, send ↑ / stop ■.
- Mawas Memory lives in the left sidebar (a **Memory** row under Mawas, ⌘K *Mawas: Memory*, Today's *Memory ›*): proposals first, entries in one column, edit inline, full keyboard; a saved Memory tab moves there once.

### Fixed
- Popover buttons over a dock edge take the click again; `○ Stopped · Start` starts the daemon in one click and the stopped state is said once; Undo after closing a split puts it back in its split; toasts keep their text and height; the unread dot is visible and centred; a zoomed split says so (`⊞ 3 · zoomed`); rail tooltips follow reordering; the status-bar branch follows the visible tab; one-line session rows aged by last activity; one UI typeface everywhere except code.
- One session, one tab: opening a Mawas session from anywhere focuses Mawas Home instead of a second tab; the same title and icon everywhere; saved layouts with duplicate tabs are merged.
- Resources panel: fixed columns with ellipsis and tooltips, no overlapping text at any width, session titles instead of raw ids, one header.

## 0.2.0 — 2026-10-22

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
- Clearer approvals (one primary *Allow once*), chat retry (a rate-limited turn counts down to *Retry now*), mute per session, the agent's real mode shown; drafts survive, a context meter in the chat header, mark unread and snooze.
- Mawas keeps a curated memory: entries you approve, with revisions; `hutan memory`.
- Models: display names everywhere (also after a restart, kept offline); pickers mark the current model, order by family and newest first, and tell apart same-named variants.
- Language servers: the Problems panel shows each server's state with Restart for a failed one; the trust bar names every server the folder would start and one answer covers the folder; a rustup `rust-analyzer` without the component is reported as not installed with the install command.

### Workspace and look
- Quiet, consistent visual design: new tokens and primitives, title bar and tabs, sidebar, chat, Agents page, Mawas Home and Settings sheet redesigned; keyboard-only focus ring.
- Two-step first run; Start sheet with pinned primary button; empty states; theme and icon pickers with live preview and `hutan theme` commands.
- **Setup** you can run any time (Hutan menu, Settings → General, palette, `hutan setup`): one flow with your current answers filled in — agents, Vim and the send key, history, background service, appearance; skipping a step keeps its value.
- After a reinstall Hutan offers to run the setup again (optional; projects, sessions and settings stay); after an update it shows *What's new* from this changelog.
- Terminal: distinct names with inline rename, link hints, drop and paste of files, resize fixes, jump to latest, command toolbar off the command line.

### Reliability
- The app manages its background service: it knows which build answers, restarts an idle older or foreign service onto its own binary (asks first when sessions are live, never replaces a newer one), retries a failing start with backoff and stops after three identical failures with a *Retry*, and a stop from any place (app, menu bar item, `hutan daemon stop`) stays stopped until you start it. Start-at-login is re-registered when the app moved.
- Daemon menu: Start/Stop/Restart/Status are enabled whenever they can act (also with no Hutan window in front); recovery items show only while their failure is current; *Start Daemon at Login* no longer claims the launch agent is missing when it simply was never registered.
- When the background service cannot start, Hutan says why and offers the fitting action: data written by a newer Hutan → update command or *Start with empty data…* (old data kept in `backups/`); a stuck service → show or stop that process; anything else → logs and details. `hutan daemon status|why|reset-data`.
- Many flaky tests fixed at their cause; a guard fails the test run if a test leaves a daemon behind.
- File watching no longer misses a write made between reading a file and watching it.

## 0.1.0 — 2026-10-19

First preview build (pre-release): the `hutand` daemon, the desktop app and the `hutan` CLI for macOS on Apple Silicon. Terminal sessions with shell integration, agent chat sessions for Codex, OMP and Claude Code, Mawas the assistant, approvals, worktrees and tasks, history search, themes. Ad-hoc signed, not notarized. Install: `brew install --cask robertt3kuk/hutan/hutan`.
