# Hutan end-to-end usability pass (build of master 4dfeeef, debug)

Setup: fresh `HUTAN_HOME=~/hutan-scratch/e2e/home`, history index off, bins copied to `~/hutan-scratch/e2e/bin/`. Scratch repo `~/hutan-scratch/e2e/acme-api` (git, bare remote `remote.git`, staged+unstaged+untracked changes). Fixture agents: `hutan-acp-fixture replay` with my own transcripts in `~/hutan-scratch/e2e/fx/` (gen.py): `omp` = queue/stop scenario with `hold` entries, `claude` = permission + three file edits (the file write on disk is done by me while the agent is held at the diff event, so the daemon sees `fs.changed` right after the `file_diff`). Window 1280x800 pt, screenshots 1366x854. Shots: `~/hutan-scratch/e2e/shots/NN-*.png`.

Environment caveats (not Hutan bugs, but they shape what was verified):
- The Mac's input source is **Russian**; Cua key presses therefore produce Cyrillic. I switched to ABC for the Vim/editor tests via TIS and restored Russian at the end. Vim results below are with ABC.
- Cua `type_text` sends text events, not key events, so Vim Normal-mode commands only work via `press_key`/`hotkey` (I used per-key presses).
- **⌘-click on terminal output could not be verified**: the driver's modifier click did not register as ⌘ (a ⌘-click on the sidebar also behaved like a plain click). `terminal_view/view.rs:1431` shows the code path (`event.modifiers.platform` → detected link → `OpenLink`), so I could not test it. Opening a file at `path:line:col` was checked with `hutan open` instead (works: file tab at 12:5).
- Cua cannot type into the native NSOpenPanel (⌘⇧G ignored); I navigated it by clicking and Return. First-run "Open a folder" itself worked.
- Checkpoints: engine runs (`home/checkpoints/` exists) but this build (4dfeeef) has no UI and no `hutan checkpoint` command (CK-3 landed after). Nothing visible to review.
- Not exercised: Follow switch (my fixture's third edit came with Follow accidentally toggled off), Codex harness, light-theme tour of every screen.

## Top 10 issues by user impact

1. **No way to open a file by name.** ⌘P is "Jump to" (workspaces/projects/sessions); typing `main.rs` gives "Nothing matches". The only ways in are `File: Open File…` (native Finder dialog, which starts at its last location, not the project), ⌘-click in a terminal, a chat card's Open, or `hutan open`. Zed/Cursor/VS Code all give ⌘P = fuzzy file finder. (major, editor/palette; shots 12, 13, 14)
2. **"Needs you" / `review · awaiting review` never clears and is wrong while the agent works.** After the first turn ends, the chat header says "review · awaiting review", the sidebar grows a "Needs you" group, Agents badge and status bar say "1/2 need you". When a queued message starts the next turn (and during Stopping…, and while turn 3/4 run, and while I am looking at the chat) it stays "review". It never went away through the whole session, even after I replied in the chat and after Finish committed everything. Pure noise in a product whose promise is quiet attention. (major, daemon attention state / chat header; shots 58, 58b, 60, 62, 64, 79, 100)
3. **Vim: status bar is stale.** The mode chip stays "NORMAL" while in Insert mode (block caret turned into a bar, text being typed), and `Ln/Col` only update after a mouse click or `:`; keyboard motions (`w`, `0`, `j`) leave "Ln 12, Col 16" frozen. (major for Vim users, editor status; shots 22b, 24b, 43, 33b)
4. **Vim: `/` search has no prompt.** Typing `/load` shows nothing anywhere (no search line, no echo); `:` does show an ex line. In real Vim and Zed you see what you type. (major, editor vim host; shot 33b)
5. **Vim with a non-Latin layout is unusable**: Normal-mode commands match `key_char`, so with a Russian layout every command key is a Cyrillic letter and does nothing (only digits work). Real Vim has `langmap`; Zed maps by physical key for bindings. (major for such users, `code_view/modal.rs:118`)
6. **Vim `:sp` quirks.** The new split starts at line 1 (Vim keeps the cursor line); in the lower pane the ex line is not visible at all and `:q<CR>` typed there did not close the pane, while the same in the upper pane closed it. (minor→major, editor splits; shots 35, 36, 36g, 36i)
7. **Terminal link discoverability.** A plain click on `src/main.rs:16:` does nothing and gives no hint; only ⌘ shows underline (spec). The block's hover toolbar ("Copy command / Copy output / Collapse / Rerun / Send to agent") overlays the end of the command line (covers `head -8`). (minor, terminal; shots 10, 11)
8. **Finish message is taken from the wrong chat.** The commit box pre-fills "Look around the project and tell me what you find." (title of the first chat, which changed nothing) while the Claude chat did the edits. Commit + push itself worked (verified with `git log`, remote has `2cd02d6`). (minor/major, Changes/Finish; shot 93)
9. **Start composer/launcher clutter.** First-run composer: switching *What* to Plain terminal hides the Prompt and Agent rows and the whole form jumps up; the Title field placeholder "What it is called in the Tasks panel (from the prompt)" and "Queue as task / Starts the task now" make no sense for a terminal; with Options open the primary button is cut off at the window bottom (shot 06); the launcher warning "Another live agent works in this checkout: … Pick New worktree to keep th…" is clipped; the Mac-style Open panel is not rooted at the project. (minor, launcher; shots 06, 07, 70)
10. **Wording/visual nits in the chat queue and agent bar**: chip button says "Interrupt & send" even when nothing runs (after Stop); the queue × buttons sit ~10 px lower than their rows and the rows are loudly tinted orange; the agent bar reads "Change the greeting and simplify total(). changed 2 places in this file, on disk" (chat title + stray period, not the agent name); conflict bar says only "store.rs changed on disk" (not that an agent did it) and its diff has no legend of which side is yours; Accept All makes the text jump up 40 px. (polish; shots 54, 57, 75, 80, 81, 87)

## Bugs (facts)

### B1 [major] Needs-you state stays "review" (see top 2)
Steps: Start OMP chat (fixture), let turn 1 end, a queued message starts turn 2. Expected: header "working", no Needs-you group. Actual: header "review · awaiting review" with eye icon, sidebar "Needs you", status bar "1 needs you" — throughout turns 2–4 and Stopping…. Same for the Claude chat from its second turn. Evidence: shots 58, 58b, 60, 62, 64, 79.

### B2 [major] Vim status bar stale (top 3)
Steps: Settings → Editing → Vim mode On; open `app.py`; click line 12; `c` `i` `w`. Expected: INSERT chip, updated position. Actual: caret becomes a bar but chip says NORMAL, "Ln 12, Col 7" constant; after click on line end "Col 16" on a 15-char line (caret past the last character in Normal mode). Evidence: shots 22d–22j, 24, 24b.

### B3 [major] Vim `/` shows no prompt; `:sp` then lower-pane ex line missing (top 4, 6)
Evidence: shots 33b, 35, 36g.

### B4 [minor] Vim toggle in Settings captured by the settings search field
Turning Vim on while the Settings sheet is open: the first Escape puts the *Settings search field* in NORMAL (status bar shows NORMAL), the second closes the sheet; afterwards the editor had no focus caret until clicked. The row's secondary line shows only "You" (clipped "Set by you"?). Shots 20, 20b, 20c.

### B5 [minor] Send now / Stop feedback inconsistency
Stop menu → "Stopping…" shows in the composer (shot 60). ⌃↩ "Interrupt & send" while the agent is slow shows only a warning line and keeps the normal stop button — no "Stopping…" (shot 63b). Both end correctly (shot 64).

### B6 [minor] First-run residue
Opening `~/hutan-scratch` (wrong folder, by my dialog click) then `acme-api` removed the first project from the sidebar, but it stayed in the ⌘P list (`hutan-scratch ~/hutan-scratch`, shot 12). Nav row "Tasks" appears only after the first terminal (layout shift of the whole nav, shots 05 → 08). File opened while "Home" is the active project is attributed to Home in the sidebar (shot 16).

### B7 [polish] Empty center after closing the last tab is a blank dark pane titled "Hutan" (shot 25).

## Friction

- Editor has Vim off by default and the setting only exists as a Settings row (no first-run question for it; the first run asks "Which key sends a message?", shot 05). `Vim uses the system clipboard` defaults to **On** (every `dd` overwrites the clipboard) — differs from Vim and Zed's default.
- Reload on the conflict bar discards unsaved text without a confirmation (⌘Z does bring it back: shot 83) — good that it is recoverable, but it is not said.
- Settings sheet: 18+ sections in a list that scrolls; "Hutan Light/Dark" strings repeated; fine but long. Changes right-panel row says only "main ~/…/acme-api · main checkout · unc…" with no file count; the full view has it.
- Chat header shows the raw model id `gitlab-duo/duo-chat-opus-4-6`; "⚠ main" pill on session rows is cryptic (it is the shared-checkout warning).
- Status bar line "1 other live agent(s) already work in /Users/…: Look around… (01M5FQ…); their edits and this task's can overwrite each other" is a long raw sentence that overflows the bar (shot 71).
- Mawas Home "Today" column shows "Working · State unavailable · 1" and nothing under *Needs you* even though 2 agents need me (shot 102).
- Time to open: terminal ready ~3 s wall including driver overhead; file tab via `hutan open` ~1 s; CPU idle 0.1 %, desktop RSS 73 MB, daemon 30 MB after a 30-minute session.

## What works well (praise)

- First run: no modal; agent readiness row (OMP ready, Claude Code ready, Codex not found + Install…) → Open a folder → inline start composer with the project and branch. Two steps, as designed (shots 01, 05). The one-line "Which key sends a message?" is the right place for that question.
- Terminal: command blocks with exit time badges, git colors, shell integration prompt. `ls`, `grep -rn` render cleanly; `path:line` detection underlines `src` in the command (shot 09).
- Editor: tree-sitter highlight on open, ⌘S/⌘Z/⇧⌘Z with correct dirty dot (clean after undo back to saved), Vim basics correct: `ciw`, `dd`, `.`, `u`, `gg`, `jjjj`, `5G`, Ctrl-V block + `I# `, `qa…q 3@a @@`, `:w` (disk verified), `:sp`, `:q` in the upper pane, `/usage<CR>`.
- Chat queue: chips with Edit (message moves into the composer, "esc keeps it as it was"), × remove, Clear, order on Send now (new message before older queued), held queue after Stop with the sentence "Waiting: you stopped the turn. Send one now to go on.", Stop menu "Stop and clear the queue (1 message) / Stop, keep the queue", "Stopping…" (shots 54–64).
- Approval card: Allow once primary, Always allow / Reject / Always reject with one honest explanation sentence ("Always sets a standing rule … that Hutan cannot show or revoke"), answered state "Allowed once · from this window", tool card +2 −6 with Open (shots 71, 74).
- Agent edits in an open file: clean buffer reloaded with tinted lines and gutter bars, per-hunk Reject (⌘⌥Z) put the base back and wrote the disk, ⌘Z undid it; a new turn replaced the previous turn's hunks; Accept All clears the bar. Dirty buffer got the conflict bar (Reload / Keep Mine (Overwrite) / Show Diff) and no text was lost (shots 75–87).
- Changes view: file list with `unstaged` / `both` markers, staged/unstaged/all chips, `+/−` per file and total, Finish card that states exactly what it does (untracked included, upstream, no force, hooks not run); commit and push verified (`git log` + remote). Done state with the mascot (shots 92–94).
- Agents page: one state word per row ("Needs you · review · Review changes"), project/checkout, harness/model, run time, "Show changes". Quiet and readable (shot 100).
- Light theme switch is live and complete (shot 102); narrow window (760 pt) collapses the sidebar to an icon rail and keeps Mawas composer and Today column usable (shot 103).
- Launcher confirm for the first prompt ("This sends your prompt to OMP · … now", "Don't ask again for this model") is a good paid-model guard (shot 52).

## Comparison notes
- File finder: Zed/Cursor/VS Code ⌘P = files; Hutan ⌘P = sessions/projects (documented as Zed's "quick open" for sessions) — biggest muscle-memory break.
- Queue: Claude Code / Codex apps queue or steer too; Hutan's visible queue chips with edit/remove are better than Claude Code's invisible queue.
- Agent edit review: Cursor/Zed show hunk accept/reject inline per hunk; Hutan's bar + ⌘⌥Z is lighter and its Reject saves to disk, but the Accept side is only "Accept All".
- Git: Zed has a staging panel; Hutan shows staged/unstaged read-only plus one Finish. Simpler, but no way to commit a subset.

## Cleanup
Desktop quit, `hutan daemon stop --stop-sessions` run twice (daemon had restarted once after the desktop was killed); `pgrep -fl hutan-scratch/e2e` is empty. Input source restored to Russian, appearance set back to dark, window frame restored.
