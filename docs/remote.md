# Remote hosts over SSH (R-2, ADR 0022)

> **Experimental — use at your own risk.** Remote hosts ship switched off. They may change or break between releases, and anyone who can SSH into the host's account has the same control over its Hutan as the user of this Mac. They are off until you turn them on:
>
> - **Settings ▸ Experimental features ▸ Remote hosts over SSH (experimental)**, or
> - add to your `settings.toml` (user file only; a workspace's or project's `.hutan/settings.toml` cannot switch it on):
>   ```toml
>   [experimental]
>   remote_hosts = true
>   ```
>   or `hutan ui set experimental.remote_hosts true` while the desktop is open.
>
> Turn it off the same way (`false`, or delete the key). While off, `hutan --host …` and `hutan host …` refuse to run (exit 2) and name the setting; nothing connects. `hutan proxy` is not gated and is hidden from `--help`: it is what `ssh` runs on the host, which only happens when someone already has SSH access to that account. `hutan host add` and `hutan host test` repeat the disclaimer in their output.

`hutan --host NAME <command>` runs any CLI command against the `hutand` of another machine. Hutan opens no listener and stores no credential for this: the connection is the system `ssh`, running `hutan proxy` on the host, and the protocol (ADR 0002) runs unchanged over its standard input and output. Authentication, host keys, jump hosts, agents and connection sharing are the user's `~/.ssh/config`.

```
hutan --host studio ls        ssh -T … studio 'hutan proxy'        hutand (on studio)
   client  ── frames ──▶  ssh stdin ═══ SSH channel ═══▶ proxy ── Unix socket ──▶
   client  ◀── frames ──  ssh stdout ◀══ SSH channel ═══ proxy ◀─────────────────
```

This slice is the CLI and the client library. The desktop's multi-host UI, host identity and native notifications for remote intents are R-3; phones and the web listener are later slices of the ADR.

## Set up a host

0. **Turn the experiment on** (above), on the Mac you run `hutan` from. The host needs nothing turned on: it only runs `hutan proxy`.
1. **Install Hutan on the host** (a Mac for now; the Linux daemon is LX-1). The host needs the `hutan` and `hutand` executables side by side (the proxy starts `hutand` beside itself) and `git`, and whichever agent harnesses you use, logged in *on the host* (their logins stay in their own homes there; Hutan never forwards a token, ADR 0015).
2. **Make `ssh HOST` work without a prompt.** Hutan runs `ssh` with `BatchMode=yes`: a password, a passphrase prompt or an unknown host key is a failure, never a question. Put the destination in `~/.ssh/config` (keys, `ProxyJump`, an agent, `IdentityFile`), and connect once by hand so the host key is known:

   ```
   Host studio
     HostName studio.local
     User me
     IdentityFile ~/.ssh/id_ed25519
     # Optional: one connection shared by every Hutan command (cold connect ~100 ms, warm ~60 ms)
     ControlMaster auto
     ControlPath ~/.ssh/cm-%C
     ControlPersist 10m
   ```

3. **Add the host.** `hutan host add studio studio` (name, then what `ssh` is given: `user@host`, a `~/.ssh/config` alias or an `ssh://user@host:port` URI). If `hutan` is not on the *non-interactive* `PATH` of the host (typically it is not: `/opt/homebrew/bin` and `/usr/local/bin` are set up by login and interactive startup files), give its path: `hutan host add studio studio --hutan /opt/homebrew/bin/hutan` (a leading `~/` is expanded by the host's shell). This writes a `[[host]]` into your `settings.toml` (comments in the file stay):

   ```toml
   [[host]]
   name = "studio"                       # letters, digits, . _ -  (what --host takes)
   ssh = "studio"                        # one word; options belong in ~/.ssh/config
   hutan = "/opt/homebrew/bin/hutan"     # optional
   ```

4. **Check it.** `hutan host test studio` connects, handshakes and prints the time to connect, the median round trip of `daemon.status` and what answers (version, protocol, pid, uptime, sessions). Then `hutan --host studio ls`.

`hutan host list` shows the profiles (`ls` is an alias), `hutan host remove NAME` removes one. Profiles belong to the user's `settings.toml` only: a `[[host]]` in a workspace's or project's `.hutan/settings.toml` is refused with an error (a repository must not choose which machine a command runs on).

## Using a host

```
hutan --host studio ls
hutan --host studio new --cwd /Users/me/proj -- zsh      # an absolute path on the host
hutan --host studio attach t:1                           # the terminal, over the same ssh
hutan --host studio chat …  agents  ps  approvals  fs …  # every command that talks to a daemon
hutan --host studio daemon status
```

- **One ssh per command**, running `hutan proxy`; it ends with the command. If the host's daemon is not running the proxy starts it there, like any client on that machine, so the first command after a reboot takes a second or two longer (measured below).
- **Paths are the host's.** The daemon resolves every path on its own machine, so a command that would default to the current directory (`hutan new`, `hutan fs ls`, `hutan wt new` from a checkout, …) or a relative path is refused with `give an absolute path on the host`: your current directory is not a directory there. `$HUTAN_WORKSPACE` and the session you run in (`HUTAN_SESSION_ID`, the agent origin of ADR 0016) are those of the local daemon and are not sent.
- **Refused with `--host`** (exit 2, before any ssh): `daemon start|stop|restart|why|reset-data` (the host's daemon is started by any connecting command and is stopped on the host itself, `ssh studio hutan daemon stop`; nothing here sends `daemon.shutdown` to a host by accident), `mcp` (an MCP server belongs to the agent that runs it: configure the agent to run `ssh studio hutan mcp`), the agent hooks (`hutan agent …` except `interrupt`), `integration`, `theme`, `help`, `host` and `proxy`. `--host` and `--socket` exclude each other.
- **Exit codes** are the CLI's: 3 when the host cannot be reached (what `ssh` or the host's shell printed is the message), 1 for the daemon's own errors.
- `ssh` is `ssh` from `PATH`, or the program in **`HUTAN_SSH`** (a wrapper that adds `-F`, `-i` or an option; the tests' fake `ssh`).

### What Hutan passes to ssh

`ssh -T -a -x -o BatchMode=yes -o ClearAllForwardings=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -- DESTINATION 'hutan proxy'`

No terminal (no PTY, so no shell echo or line editing can touch the bytes), no prompt, no agent or X forwarding, and the port forwards of your `~/.ssh/config` are cleared for these connections (they belong to your interactive sessions; two connections must not fight for the same port). The destination follows `--` and a destination that starts with `-` or has whitespace is refused, so a settings file cannot smuggle an `ssh` option. A dead link is noticed within about a minute (`ServerAlive*`) and `ssh` exits. `ssh` runs in its own process group, so Ctrl-C reaches `hutan`, which ends it.

## Reconnecting

The client library supervises an SSH endpoint like a local one: `Connection` (what the desktop uses) dials `Endpoint::Ssh(host)` and, when `ssh` exits (a dropped link, a sleeping laptop), starts a new one and reconnects with `hello.resume`; what happened meanwhile is replayed (or a snapshot is taken when the daemon restarted), as measured in the remote spike (23 frames in 0.6 ms). The CLI makes one connection per command and does not reconnect inside one (`hutan events --follow` ends when the link does).

## `hutan host forward NAME`: for tools that speak the protocol themselves

```
hutan host forward studio                 # prints the local socket, runs until Ctrl-C
HUTAN_SOCKET=~/Library/Application\ Support/Hutan/hosts/studio.sock hutan ls
```

Runs `ssh -N -T -L LOCAL:REMOTE -o StreamLocalBindUnlink=yes -o ExitOnForwardFailure=yes …` for you: `StreamLocalBindUnlink` replaces a socket a previous run left behind (without it ssh fails with `Address already in use`), and `ExitOnForwardFailure` ends `ssh` instead of leaving a connection that forwards nothing. The local socket is `hosts/NAME.sock` next to the local daemon's socket (created `0700`/`0600`; `--listen PATH` overrides it; a Unix socket path is limited to about 100 bytes). The host's socket path is asked from the host (`hutan daemon start --json` there, which also starts its daemon); `--remote-socket PATH` skips that. If `ssh` exits (the link drops) it is started again after 1 s, 2 s, … up to 30 s; five failures in a row (nothing stayed up for a minute) end the command with exit 3. Ctrl-C (or SIGTERM) stops `ssh` and removes the socket.

A forwarded socket is a way for *any process of your user* on this Mac to talk to that daemon, with the identity of a local client: it is for the tools you start yourself. Prefer `--host`, which gives nothing to other processes.

## Security

- **SSH is the authentication and the encryption.** There is no Hutan listener, port, token, certificate or key for host access; the host's `sshd` is the only door and the host's account is the identity. Whoever can `ssh` into the account can run `hutan proxy`, i.e. is that user to the daemon (the daemon's peer check still applies on the host: the proxy is a local process of the user). Restrict it as you would any shell access (`authorized_keys` options, `Match` blocks, a dedicated account).
- **Host keys fail closed.** With `BatchMode=yes`, an unknown or changed host key stops the connection and the message from `ssh` is shown (`Host key verification failed.`); Hutan never accepts a key on your behalf and sets no `StrictHostKeyChecking`: your configuration decides (`accept-new` if you want it).
- **No credential is stored or forwarded by Hutan**: not a password, key, passphrase or token. A secret entered on the host (a provider key, a harness login) stays there (ADR 0008, 0015); nothing in the CLI sends the local Keychain's content to a host.
- **A host is trusted with its own data.** Everything the host's daemon returns (sessions, files, transcripts) is shown as it is; the host can show anything to your CLI.
- **Destinations and the remote command are quoted**: the destination cannot start with `-`, and the `hutan` path is quoted for the host's shell (POSIX and fish), so a profile cannot run another command.
- Settings problems (a bad `[[host]]`) are reported with the file and entry (`hutan host list`, the desktop's settings report).

## Limits

- **Mac to Mac only** until LX-1: a Linux host has no `hutand` yet.
- **The daemon of the host posts its own notifications** (and only from a signed app bundle); a CLI does not show them. Local notifications for remote intents, the host's identity in the UI and several hosts in one window are R-3.
- Latency is the SSH link's: a request costs one round trip (measured: +60–90 µs over loopback). Terminal output is ack-paced (one render per RTT), so a high-latency link shows fewer, larger updates; typing in a full-screen program feels the RTT.
- **No resume inside one CLI command**, and `hutan attach` ends when the link does.
- One byte of text on the host's standard output before `hutan proxy` starts ends the connection (below).
- **Files from this Mac are not copied into a remote project yet** (EX-8, the desktop's Files panel): a file dragged in from Finder, or pasted from it, reaches the daemon as a path chosen on this Mac (`fs.copy` with `picked_from`), which the host would read on *its own* disk. The desktop refuses with that reason, shown in the notice area, whenever the window's connection is `Endpoint::Ssh`; uploading the bytes through `fs.write` is the later step.

## Troubleshooting

| Message | Cause and fix |
|---|---|
| `remote hosts are off.` (exit 2) | `experimental.remote_hosts` is false: turn it on (top of this page). |
| `ssh studio: Permission denied (publickey).` / `…Host key verification failed.` | `ssh studio true` does not work non-interactively: fix the key, the agent or the host key in `known_hosts` (connect once by hand). |
| `ssh studio: <shell>: command not found: hutan` | `hutan` is not on the host's non-interactive `PATH`: `hutan host add … --hutan /full/path/hutan`. |
| `ssh studio: the host printed text before hutan's first message (`…`)` | A startup file of the host's shell writes to standard output (a banner, `echo`, `fortune`, `.zshenv`, a `.bashrc` without an interactive guard). Make non-interactive shells silent: in bash put `[[ $- == *i* ]] || return` at the top of `.bashrc`; in zsh keep output out of `.zshenv`; remove banners and `echo`s from files a non-interactive shell reads. |
| `ssh studio: hutan: cannot connect to hutand at …` | The proxy could not start the daemon (`hutand` not beside `hutan`, or it failed): run `ssh studio hutan daemon why`. |
| `ssh studio: no answer within 60 s` / `timed out waiting for welcome` | The link or the host is stuck; `ServerAlive*` ends a dead connection in about a minute. Try `ssh -v studio true`. |
| `cannot run `ssh`` | `ssh` is not on `PATH`; set `HUTAN_SSH`. |

## Verified

Automated (no sshd in the gate): `hutan proxy` over an in-memory pipe pair and as a real process whose standard output is read by the client (control, render and input frames; end of input and a closing daemon end it; nothing on standard output but frames, nothing on standard error); `Endpoint::Ssh` against a fake `ssh` script that records its arguments and execs `hutan proxy` (arguments, no autostart, the error texts above, resume with replay, reconnect after the `ssh` process is killed); the CLI's `--host` routing, refusals, `host add|list|remove|test|forward` and a PTY `attach` through a host (`crates/cli/tests/it/{proxy,ssh_host,hosts,attach}.rs`, `crates/client/src/ssh.rs`, `crates/settings/src/hosts.rs`).

By hand (2026-10-26, a throwaway user-level `sshd` on `127.0.0.1:2271` with its own host key, client key and `known_hosts` in `~/hutan-scratch/r2/`, a scratch `HUTAN_HOME` for each side, the host's `hutan` a two-line wrapper that sets the host's `HUTAN_HOME`; loopback, so there is no network latency):

| Step | Result |
|---|---|
| `hutan host test scratch` with no daemon on the host | 1 915 ms (ssh connect + the proxy starting `hutand` + handshake) |
| `hutan host test scratch` again | 102 ms connect, 0.31–0.40 ms median `daemon.status` |
| `hutan --host scratch ls` (whole command) | 0.11–0.12 s without `ControlMaster`, 0.06–0.07 s with a warm `ControlMaster` (`host test`: 62 ms connect) |
| `hutan --host scratch new --cwd /tmp -- sh -c 'echo ready-on-host; exec cat'`, then `attach` | session created in 0.15 s; the screen arrived 0.12 s after starting `attach`; a typed line was echoed back in 9.9 ms; `Ctrl-\ d` detached with exit 0 |
| `hutan host forward scratch`, `HUTAN_SOCKET=… hutan ls` | 34 ms; killing its `ssh` restarted it within 3 s and the socket worked again; a stale socket file left behind was replaced; Ctrl-C removed the socket and its `ssh` |
| host key changed / unknown key | `ssh scratch: Host key for [127.0.0.1]:2271 has changed and you have requested strict checking.; Host key verification failed.`, exit 3, no connection |
| a wrapper that prints a line before `hutan` | the text is quoted in the error, exit 3 |
| `--hutan /nonexistent/hutan`, a closed port | the host shell's own message / `Connection refused`, exit 3 |
