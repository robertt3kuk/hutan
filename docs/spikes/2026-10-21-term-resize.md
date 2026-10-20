# Terminal resize: why prompts corrupt, and the reflow decision (UX-TERM-RESIZE, phase A)

Question: after a few width changes the audit saw stacked prompt fragments and chips off their rows (`2026-10-21-ux-audit.md` F2, **major**, "every dock toggle is a resize"). Is it Hutan's reflow policy, and what should the policy be? Evidence below was gathered on 2026-10-21, fish 4.9.3, zsh 5.9, macOS bash 3.2, on the slice branch `slice/UX-TERM-1`.

## Reproduction

1. **Real desktop, audit recipe 2** (scratch `HUTAN_HOME`, history index off): ⌘T, a command, ⌘D, ⌘⇧D, ⌘B, ⌘L, ⌘J, ⌘J, ⌘L, ⌘B. The first pane's prompt ended as `…omies-MacBook-Phomie@homies-MacBook-Phomie@homies-MacBook-Pro ~/h/Uhomi e@homies…` and `hutan history` printed the same rows, so the damage is in the daemon's grid, as the audit said. Screenshot: `~/hutan-scratch/UX-TERM-1/shots/08-f2-reproduced-before-fix.png`. The PTY saw exactly one resize per action (a `WINCH` trap in the shell logged `44×52, 44×74, 31×74, 44×74, 44×52, 44×36`): no storm, no debounce problem.
2. **Daemon only, no desktop** (`Terminal` in a PTY, the same `Emulator`): the user's login `fish -l` (config with `zoxide` and `fzf`), a prompt wider than the pane, 44 rows, widths 120→60→60→44→60→60→120→60→44→120: ends corrupted every time (3 of 3); with Hutan's shell integration on and with `fish --no-config` it ended clean in this harness (the desktop run above had the integration and still corrupted, so the harness is a weaker reproduction than the desktop, not a different bug).
3. **A recorded stream** (committed): `crates/terminal/examples/record.rs` now takes `--resize <delay>:<COLSxROWS>` and `--cwd`; `tests/corpus/recorded/fish_prompt_resize.*` is fish with a long prompt through seven width changes (recipe in the corpus README). The raw bytes show what fish does after each SIGWINCH:

   ```
   \r  [ESC[A]  ESC]133;A  …mie@homies…proj-demo>  ESC]133;B  \r\n  ESC[J
   ```

   that is: go to column 0, move up *the rows fish believes its prompt took at the old width* (`ESC[A` once when the cursor sat below the prompt's first row), draw the prompt again truncated with `…` for the new width, erase everything below.

Not reproduced: zsh (with and without the injected integration, 8 runs) and `fish --no-config` ended clean before the change (bash was only run after it); nine width changes with a prompt that fits in every width are exact (below).

## Cause

The engine reflows the soft-wrapped rows of the old prompt when the width changes, and then fish moves up the number of rows the prompt took *before* the reflow (it cannot know how the terminal broke it) and repaints. Wherever the two disagree, the up-move lands inside the reflowed rows: fragments of the old prompt stay above the new one, or an erase takes lines of earlier output with it (the recorded run lost `echo hello` and `hello`). The engine is not wrong; Ghostty's author describes the same thing for "some other popular terminal emulator on macOS": "The terminal emulator is not doing anything wrong here. It is reflowing text but because the shell doesn't (and can't, today) know that it reflowed the prompt onto multiple lines, it gets redrawn on the bottom new line and duplicates a bunch." ([Ghostty Devlog 001](https://mitchellh.com/writing/ghostty-devlog-001)).

Checked and ruled out: **a race** between the PTY resize and the engine resize (`Terminal::resize` signals the shell first). Swapping the order (engine first, locked until the ioctl returned) changed nothing: 3 of 3 runs still corrupted, and the recorder, which resizes the engine first, corrupts too. The change was not kept.

## What the reflow does on its own is exact

`crates/terminal/tests/it/reflow.rs`, no shell involved: a screen with two commands and a 62-column prompt, the audit's nine width and height changes. After every step, and after the ninth, the scrollback-plus-screen read as logical lines (soft wraps joined) equals a fresh render at that size, and the cursor is after the same prompt text. Pinned.

One difference that is *not* corruption and is not changed: narrowing pushes the top rows of a short screen into scrollback although blank rows remain below the cursor (alacritty's reflow keeps the blank rows), so a reflowed screen starts a few rows later than a fresh render of the same text; widening brings them back. Terminals that trim trailing blank rows (Ghostty) do not show this.

## How others behave

- **Ghostty** reflows output, but *not the prompt*: when shell integration says the cursor is at a prompt, a resize clears the prompt and lets the shell redraw it ("Complex prompts resize correctly by allowing the shell to redraw -- rather than reflow -- the prompt line", [shell integration docs](https://ghostty.org/docs/features/shell-integration); fish 4+ marks prompts itself, so it works without the injected script). Ghostty's own devlog names kitty as the reference for this behaviour.
- **Terminal.app** and a second run of Ghostty were **not** driven: the slice rules forbid touching other apps, and the earlier audit's attempt raised system permission prompts. Terminal.app's behaviour is [INFERENCE] only; nothing here depends on it.

## Decision

Reflow stays for everything the shell does not own (scrollback, output of finished and running commands, the alternate screen is untouched as before). **While the shell sits at a prompt (between `OSC 133;A` and `C`) a width change erases the prompt instead of reflowing it**: the rows from the prompt's first row to the bottom are erased and the cursor is put back at the same distance below the prompt's first row it had. The shell's repaint then moves up exactly the rows it expects and draws on clean rows.

The distance matters. The first attempt put the cursor on the prompt's first row; fish's `ESC[A` then climbed into the previous output row, so every resize ate one line (measured: six resizes, one line left). The distance below the prompt's first row *before* the resize is by construction what the shell believes (the screen matched its layout until then), so keeping it is what the shell expects, for fish and for any shell that repaints by moving up. Gate: the same marks the block overlay already uses (`BlockIndex` phase *Prompt*), so only a shell that announces its prompts is affected: fish 4 (native marks), and zsh/bash/fish started by the daemon with the injected integration. A prompt that starts above the screen is left to the reflow.

This is implemented in this slice because it is 25 lines in `Emulator::resize`, touches nothing else, and the recorded fish stream, the scripted shell tests and the real desktop all go from corrupted to clean:

- `crates/terminal/src/emulator.rs` `clear_open_prompt`, `blocks.rs` `open_prompt_line`.
- Tests: `reflow::a_prompt_the_shell_repaints_stays_clean_through_width_changes` (a scripted shell that repaints by moving up its old row count; every one of the nine steps equals a fresh render's prompt rows and nothing sits below the cursor; fails without the change), `reflow::without_prompt_marks_the_reflowed_prompt_and_the_repaint_stack` (no marks → today's behaviour, pinned), the corpus golden `fish_prompt_resize` (re-recorded with the new engine; fish's repaint depends on the cursor the engine reports).
- By hand (throwaway harness, not committed): zsh and bash with the injected integration through seven width changes with a prompt wider than the pane end with one prompt at the final width and the earlier output intact.
- Real desktop: audit recipe 2 again, `~/hutan-scratch/UX-TERM-1/shots/11-after-recipe-fixed.png`; `hutan history` of the session shows one prompt.

## Not done, for phase B (wave 2)

- Shells that send no marks (`HUTAN_SHELL_INTEGRATION=0`, other shells): still reflow and still stack. The remedy there is the *Terminal: Redraw* command below, not a guess.
- A shell that announces a prompt but does not repaint on SIGWINCH would show a blank prompt until the next key. fish, zsh and bash (readline) all repaint here; a shell that announces prompts without redrawing is [INFERENCE]. A mark flag (kitty and Ghostty have a `redraw` option on `OSC 133;A`) would make it explicit.
- Not tested: two-line prompts and right prompts (starship, `RPROMPT`) — the distance rule should cover them; add recorded streams.
- Blocks: `blocks.remap` is untouched; check that chips of earlier blocks still sit on their command rows after reflow (the audit's second symptom) with a recorded zsh stream.
- The pushed-into-scrollback difference above; trimming blank rows before reflow would remove it.

## *Terminal: Redraw*

A palette command (`workbench::RedrawTerminal`, `hutan ui run workbench::RedrawTerminal`) asks the program in the focused terminal to repaint. TIOCSWINSZ with an unchanged size sends no SIGWINCH, so the view reports the size with one row less (never one column: that would reflow) and, 150 ms later, the real size (the layout's, if it changed meanwhile). Checked: a shell with a `WINCH` trap logged `43×36` then `44×36` and the prompt stayed clean. It repaints what the program draws; it cannot repair rows that are already garbled in the grid.
