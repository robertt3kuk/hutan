# Recorded corpus

Raw PTY output of real programs (`NAME.raw`, exact bytes the program wrote with
`TERM=xterm-256color`, at 80×24 unless `NAME.size` (`COLSxROWS`) says otherwise) and the final
screen as text (`NAME.grid`, produced by replaying the raw bytes through a fresh `Emulator`).
`tests/it/corpus.rs` replays every `.raw` (whole, in 4096-byte chunks and byte by byte) and
compares the grid text with `.grid`. A recording made with `--resize` also has `NAME.resizes`
(`OFFSET COLSxROWS` per line: the engine is resized when the replay reaches that byte offset,
which is where the resize happened in the live run).

The recorder answers the program's terminal queries (DA, DSR, kitty, …) through a live engine
while recording, so the programs behave as in a real terminal (colour queries are answered with
the default colours); the replies are not stored.

Re-record (from the repository root):

```sh
R="cargo run -p hutan-terminal --example record --"
$R zsh_printf_ls -- zsh -f -c 'printf "\033[1;31mred\033[0m \033[38;2;10;20;30mrgb\033[0m \033[4mul\033[0m\n"; printf "wide: \xe7\x95\x8c\xf0\x9f\x98\x80 e\xcc\x81\n"; ls -G -d /bin /usr/bin/ca* /bin/cp'
$R zsh_seq_scroll -- zsh -f -c 'seq 1 60'
$R vim_edit_inserted --send 800:'ihello hutan\e' --timeout-ms 2000 -- vim -u NONE -n   # frozen mid-edit, alternate screen active
$R vim_edit_quit --send 800:'ihello hutan\e' --send 400:':q!\r' -- vim -u NONE -n     # full round trip, primary screen restored
# fish_prompt_resize (UX-TERM-RESIZE, audit F2): fish 4 with a long prompt, seven width changes. The
# recording is made at HOME=/tmp/fish-home holding `.config/fish/config.fish`:
#   if status is-interactive; command -q zoxide; and zoxide init fish | source;
#     command -q fzf; and fzf --fish | source; end
#   function fish_prompt; echo -n -s "developer@dev-workstation " (prompt_pwd) "> "; end
# (under `fish --no-config` fish repainted nothing after the resizes, so no stack) and `USER=dev`; run from a
# deep temporary directory (`--cwd`, e.g. /tmp/x/aaaa/bbbb/cccc/dddd/eeee/a-long-project-folder-name-demo)
# so the prompt is wider than 60 columns. Afterwards replace the machine's own host name in the OSC 7 URLs
# with a same-length one (`dev-workstation-host.lan`).
$R fish_prompt_resize --size 100x24 --cwd "$DEEP_DIR" --env HOME=/tmp/fish-home --env USER=dev --timeout-ms 20000 \
  --send 1500:'echo hello\r' --resize 900:60x24 --resize 900:44x24 --resize 900:60x24 --resize 900:100x24 \
  --resize 900:44x24 --resize 900:100x24 --resize 900:60x24 -- /opt/homebrew/bin/fish -l
# zsh_two_line_resize (UX-TERM-RESIZE phase B): zsh 5.9 with Hutan's injected marks (ZDOTDIR shim and
# HUTAN_SHELL_INTEGRATION_DIR from crates/terminal/shell-integration, laid out as the daemon installs it:
# $ROOT/zsh/.zshenv + hutan-integration.zsh), a two-line prompt and a right prompt. $ZDOT/.zshrc:
#   HOST=devbox; USER=dev; PROMPT=$'dev@devbox %~ on main\n❯ '; RPROMPT='[12:00:00]'; HISTFILE=/dev/null
$R zsh_two_line_resize --size 100x24 --cwd "$DEEP_DIR" --env HOME=$ZDOT --env USER=dev --env ZDOTDIR=$ROOT/zsh \
  --env HUTAN_SHELL_INTEGRATION_DIR=$ROOT --env HUTAN_ORIG_ZDOTDIR=$ZDOT --timeout-ms 20000 \
  --send 1500:'echo hello\r' --send 700:'printf "%s\n" $(printf "abcdefghij%.0s" 1 2 3 4 5 6 7 8)\r' \
  --resize 900:60x24 --resize 900:44x24 --resize 900:100x24 --resize 900:44x24 --resize 900:70x24 -- /bin/zsh -i
# fish_two_line_resize: fish 4.9.3 (its own OSC 133 marks), HOME=/tmp/fish-home2 with the config above plus
#   set -g fish_greeting
#   function fish_prompt; echo -s "dev@devbox " (prompt_pwd) " on main"; echo -n "❯ "; end
#   function fish_right_prompt; echo -n "[12:00:00]"; end
$R fish_two_line_resize --size 100x24 --cwd "$DEEP_DIR" --env HOME=/tmp/fish-home2 --env USER=dev --timeout-ms 20000 \
  --send 1500:'echo hello\r' --send 700:'printf "%s\n" (string repeat -n 8 abcdefghij)\r' \
  --resize 900:60x24 --resize 900:44x24 --resize 900:100x24 --resize 900:44x24 --resize 900:70x24 -- /opt/homebrew/bin/fish -l
# Agent TUI startup screens: nothing submitted, capture frozen after the timeout, then hung up.
# Run from a neutral directory (the cwd shows in the title/prompt), after building the example.
mkdir -p /tmp/hutan-rec && cd /tmp/hutan-rec
R=/path/to/target/debug/examples/record
$R omp_startup --size 100x30 --timeout-ms 3500 -- omp
$R codex_startup --size 100x30 --timeout-ms 4000 -- codex
```

The agent recordings were scanned for account emails, tokens, keys and home paths before they
were committed (the stream holds only the program's own output: banner, tip, prompt line, and
for codex the "Hooks need review" screen). Re-scan after every re-recording; the `omp` tip line
is random, so a new recording differs.

## `zsh_two_line_resize`, `fish_two_line_resize`
Real zsh (injected marks) and fish 4 with a two-line prompt, a right prompt, two commands (the second
prints a line wider than 60 columns) and five width changes at a prompt. `tests/it/reflow.rs` replays them
and checks after every resize that each block starts on the first row of its line and its output on the
row of the output's first line. Recorded with the engine that trims blank rows before a reflow.

## `fish_prompt_resize`
Seven width changes while fish 4 sits at a prompt wider than the terminal (it truncates its own
prompt with `…`, and repaints it after every SIGWINCH by moving up the rows it believes the old
one took). The grid is one command, its output and one prompt. Before the engine erased the open
prompt on a width change (`Emulator::resize`) the same run ended with stacked prompt fragments
and the `echo hello` and `hello` lines gone (audit F2, `docs/spikes/2026-10-21-term-resize.md`).
The recording must be made with the engine under test: fish's repaint depends on the cursor
position the engine reports.

## Open items
- `htop` (~1 s, then `q`) was not recorded: it is not installed on the recording machine.
  When available: `$R htop --send 1000:q -- htop`.
- Claude Code is not recorded (not installed here). Record it like the others when available.
