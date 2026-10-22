# Shell integration (OSC 133 + OSC 7)

Hutan terminals running zsh, bash or fish emit semantic prompt marks and the working directory without any edit to the user's dotfiles. The daemon's OSC sniffer (`crates/terminal/src/sniffer.rs`) turns them into `TerminalMeta::PromptMark` / `TerminalMeta::Cwd`; the block index built on top of them (`terminal.blocks`, protocol 0.5, `docs/protocol.md` § Terminal blocks) lives in the same crate (`blocks.rs`, `emulator.rs`).

Code: `crates/terminal/src/shell_integration.rs` (install + injection), scripts in `crates/terminal/shell-integration/`, wiring in `crates/daemon` (`start` installs, `registry` hands the directory to every `TerminalConfig`). Behaviour is covered by `crates/terminal/tests/it/shell_integration.rs` (real PTYs).

## What is emitted

| Sequence | When |
|---|---|
| `OSC 133;A` | prompt start |
| `OSC 133;B` | prompt end, before user input |
| `OSC 133;C` | just before a command executes |
| `OSC 133;D;<exit>` | after a command finished |
| `OSC 7;file://<host><cwd>` | on every prompt, cwd percent-encoded byte by byte |

Sequences from the scripts end with BEL. An empty Enter produces `A B` with no `D` (no command ran). Programs started inside the shell are unaffected: the marks are printed by the shell, never by children.

The marks also decide how the daemon treats a width change: between `A` and `C` (the shell is at its prompt) the prompt is erased instead of reflowed so the shell's own repaint after SIGWINCH starts from clean rows (`Emulator::resize`, `docs/design.md` § Resize and reflow). A shell that sends no marks keeps the plain reflow. Two-line prompts and right prompts (starship, `RPROMPT`, `fish_right_prompt`) go through the same rule, and a block's rows follow the reflow (zsh and fish streams in the corpus check it; UX-TERM-RESIZE phase B).

## Where it applies

The environment the shell starts with (a fresh login environment by default) is described in `docs/architecture.md` § Session environment; the integration variables are set after it and win over the client's.

Only when the session runs the login shell (empty command; `$SHELL -l`, see `default_shell`) or a command whose basename is `zsh`, `bash` or `fish`. Other programs get nothing, and variables of an outer Hutan terminal that leaked into the daemon's environment are removed from their environment (`HUTAN_SHELL_INTEGRATION_DIR`, `_VERSION`, `HUTAN_ORIG_*`, ...).

Opt-out: `TerminalConfig::shell_integration = false`, or `HUTAN_SHELL_INTEGRATION=0` in the session environment (or the daemon's). The scripts live in `$HUTAN_HOME/shell-integration/<version>/`, rewritten atomically by the daemon at start when their content differs. A shell that is running keeps what it already loaded.

Inside an integrated shell, `HUTAN_SHELL_INTEGRATION_VERSION` (currently `1`) and `HUTAN_SHELL_INTEGRATION_DIR` are exported so later slices and scripts can detect support.

## Per-shell mechanism

The user's startup files run exactly once, unedited, in the usual order; what the shell's children see is the user's original environment.

**zsh.** `ZDOTDIR` is pointed at `<dir>/zsh`, so zsh reads the shim's `.zshenv` first. It restores the user's `ZDOTDIR` (unset if there was none; the original travels in `HUTAN_ORIG_ZDOTDIR`), sources the user's real `.zshenv`, and registers a one-shot `precmd` hook. zsh reads `.zprofile`/`.zshrc`/`.zlogin` itself from the restored `ZDOTDIR`. On the first prompt, after every startup file ran, the hook loads `hutan-integration.zsh`, which adds `precmd`/`preexec` hooks with `add-zsh-hook` and wraps `PS1` in `%{…%}` marks (A before, B after; a `PS1` that already carries OSC 133 is left alone). Prompt frameworks (oh-my-zsh, starship, powerlevel10k) rewrite `PS1` in their own precmd hooks, which run before ours, so the marks are re-added every prompt.

**bash.** Started as `bash --rcfile <dir>/bash/hutan.bash …`. The shim replays what bash would have read: for a login shell (`-l`, the default) `/etc/profile` and the first of `~/.bash_profile`, `~/.bash_login`, `~/.profile`; otherwise `~/.bashrc` (or the user's own `--rcfile`); `--norc`/`--noprofile` are honoured. The flags travel in `HUTAN_BASH_INJECT`/`HUTAN_BASH_RCFILE`, which the shim unsets. It then installs `PROMPT_COMMAND` entries (first: `D` + OSC 7, preserving `$?`; last: `A`/`B` marks in `PS1`, so prompt frameworks that rebuild `PS1` are covered; string or array `PROMPT_COMMAND`), and for `C`: `PS0` on bash ≥ 4.4, a `DEBUG` trap on older bash (macOS `/bin/bash` is 3.2; an existing `DEBUG` trap is kept and chained). `--posix` + `$ENV` (Ghostty's route) is not used because bash 3.2 ignores it for startup files.

**fish.** `XDG_DATA_DIRS` gets `<dir>` prepended, so fish loads `fish/vendor_conf.d/hutan-shell-integration.fish` before `config.fish`. The snippet first hands `XDG_DATA_DIRS` back (the original value in `HUTAN_ORIG_XDG_DATA_DIRS`, or unset when there was none). fish ≥ 4.0 writes OSC 133 itself (feature `mark-prompt`: A, C, D; the prompt-end B and OSC 7 on every prompt since 4.4); the snippet only adds what the running fish lacks, using `fish_prompt`/`fish_preexec`/`fish_postexec` events. B is added by wrapping `fish_prompt` on the first prompt: the user's function is copied to `__hutan_orig_fish_prompt` and called unchanged (re-wrapped if the user redefines it).

## New splits start where the shell is (TERM-SPLITS)

The working directory the shell reports with `OSC 7` is the session's `cwd` (the daemon keeps it current, so the sidebar's folder follows it too). A **terminal split** (⌘D / ⌘⇧D with the keyboard in a terminal, or *Split Right / Down* in the terminal's context menu, `docs/design.md` § Terminal splits) starts its new terminal in the `cwd` of the split it came from, not in the folder that terminal was started in: after `cd` in an integrated shell the new split is there. A terminal without shell integration (another program as the session's command, `HUTAN_SHELL_INTEGRATION=0`) never reports one, so its split starts in the folder it was started in; a session with no folder at all falls back to the workspace's first root, then `$HOME`. Each split is a session of its own, so the next split of it follows its own `OSC 7`.

## Nested shells

The injection is consumed and undone in the integrated shell (zsh and bash functions are not exported; `ZDOTDIR`/`XDG_DATA_DIRS` are back to the user's values), so a shell the user starts inside a session gets no integration of its own. To integrate it, source the script from its startup file: `source "$HUTAN_SHELL_INTEGRATION_DIR/zsh/hutan-integration.zsh"`, `…/bash/hutan.bash`, or `…/fish/vendor_conf.d/hutan-shell-integration.fish`. The nested shell's marks arrive on the same PTY; consumers must treat unbalanced marks (an inner `D` without `C`) leniently. `ssh` and `sudo -s` sessions are out of scope.

## Limitations

- zsh: when a prompt framework sets `PS1` from a `precmd` hook, the very first prompt carries no `A`/`B` (the integration loads during that cycle, after the framework's hook ran). All later prompts are complete.
- bash is started as a non-login shell with the login startup files replayed: `shopt login_shell` is off, `logout` is unavailable, `~/.bash_logout` is not run, and `/etc/bash.bashrc` (Debian/Ubuntu builds) is not read.
- bash < 4.4 uses a `DEBUG` trap for `C`; tools that replace the `DEBUG` trap or `PROMPT_COMMAND` after startup (bash-preexec installed later, `set -T` tricks) can disable the marks. bash 3.2 is tested; 5.2 is tested when available (`HUTAN_TEST_BASH=/path/to/bash`).
- bash is only injected for plain interactive command lines (`-l`, `-i`, `--norc`, `--noprofile`, `--rcfile`, `--noediting`, `--verbose`); `-c`, scripts, `--posix` and anything else are left alone.
- A user's own OSC 133 in `PS1` (zsh/bash) disables Hutan's prompt marks for that shell.
- fish < 4.0 is expected to work through the fallback path but is not tested here; fish 4.9 uses its own marks.

## Prior art

Mechanisms were chosen after studying Ghostty (MIT) and WezTerm (MIT) and the FinalTerm/iTerm2 OSC 133 semantics; the scripts here are written from scratch (no kitty or Warp code, which are GPL/AGPL).
