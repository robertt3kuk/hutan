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
# recording is made at HOME=<scratch> holding a copy of a fish config that sources `zoxide` and
# `fzf` (under `fish --no-config` fish repainted nothing after the resizes, so no stack) plus a fake prompt
# (`function fish_prompt; echo -n -s "developer@dev-workstation " (prompt_pwd) "> "; end`) and
# `USER=dev`; run from a deep temporary directory (`--cwd`) so the prompt is wider than 60 columns.
# Afterwards replace the machine's own host name in the OSC 7 URLs with a same-length one.
$R fish_prompt_resize --size 100x24 --cwd "$DEEP_DIR" --env HOME=/tmp/fish-home --env USER=dev --timeout-ms 20000 \
  --send 1500:'echo hello\r' --resize 900:60x24 --resize 900:44x24 --resize 900:60x24 --resize 900:100x24 \
  --resize 900:44x24 --resize 900:100x24 --resize 900:60x24 -- /opt/homebrew/bin/fish -l
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
