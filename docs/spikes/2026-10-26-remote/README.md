# Remote and mobile control of Hutan — REMOTE-SPIKE

**Status:** research and recommendation. Docs only; no product code changed. Draft decision: `docs/adr/0022-remote-access.md` (Proposed).
**Slice:** `REMOTE-SPIKE` (brief `docs/briefs/REMOTE-SPIKE.md`), need #4 of `docs/spikes/2026-10-25-user-needs/README.md`.
**Builds on:** `docs/spikes/2026-10-18-remote.md` (D-3: SSH first → direct TLS → relay; transport seam, host-qualified UI, truthful status — still valid, not repeated here), `docs/spikes/2026-10-18-cross-platform.md` (Linux/Windows), ADR 0002, 0008, 0009, 0015, 0016.

## Recommendation

1. **Rank by evidence, not by the brief's order:** (1) drive a daemon on another machine from the desktop, (2) watch/approve from a phone, (3) read-only share. Reaction counts: remote host ≈ Zed #11473 930 + Codex #10450 878 + Warp #6744/#6831 79/72 + cmux #1664 38 + Superset #1513 34; phone ≈ Codex #9224 544 + Orca #3099 28 (§1).
2. **Desktop → remote daemon: system OpenSSH, same frames, no Hutan listener.** Measured on the real `hutand`: an `ssh -L` stream-local forward adds **~65 µs** at p50 (`daemon.status` 0.022 → 0.087 ms); a stdio proxy adds ~90 µs (`nc -U` stand-in); a cold SSH connect costs ~95 ms (40 ms through ControlMaster). At 50/100/200 ms emulated RTT a request costs the RTT plus the sub-millisecond transport overhead above (the emulator's own constant 4.7 ms is in the raw numbers); terminal output is ack-paced to ≈ 1 render per RTT (18 / 9.6 / 4.9 renders/s) and needs only 2.7–10.5 kB/s (§2).
3. **Phone: a minimal web app (PWA) served by the daemon, not native iOS first.** The surface is JSON control frames only (attention list, approvals, chat journal, stop) — no postcard terminal decoder. Exposure is a **loopback-only** HTTP/WebSocket listener (default off) fronted by the user's `tailscale serve` (valid HTTPS certificate, WireGuard, ACLs); Hutan binds nothing on a LAN/WAN address. Auth is **per-device pairing** (QR invite → device public key in SQLite; daemon-side private keys in Keychain) with a new **scope ceiling per device** — `observe` / `approve` / `operate`, everything else `local` — enforced in the router table next to the existing Read/Mutate/UserOnly classes (§2.5, §3).
4. **Notifications to the phone:** stopgap ntfy/Pushover through USER-HOOKS (works today, alert only); target is **Web Push straight from the daemon** to Apple's push service — verified: no Apple developer account, no Hutan-operated server, Home Screen web app on iOS ≥ 16.4 (§3.3). Native iOS/APNs and a relay service wait for demand.
5. **Linux daemon is feasible and is the real prerequisite for non-Mac hosts:** all 11 headless crates already pass `cargo check` for Linux (prior spike); the gaps are runtime — paths, process tree, login env, secret store — **≈ 8–13 engineer-days [INFERENCE]** (§4). Mac-to-Mac over SSH needs none of it.
6. **Slices (§5):** R-1 transport seam → R-2 `hutan proxy` + SSH hosts → LX-1 Linux headless (parallel) → R-3 host identity + multi-host desktop → R-4 device auth and scopes → R-5 web listener + pairing → R-6 mobile web v1 → R-7 Web Push → R-8 read-only share → R-9 WAN queue budgets → relay later under its own security ADR. Protocol impact: R-1/2 none; R-3 minor (`host.identity`); R-4 minor (`remote.devices`, `remote.scopes`); everything else rides on those.

## 0. Method, prototypes, caveats

Prototypes live outside the repo in `~/hutan-scratch/remote-spike/` (throwaway; not committed). Nothing touched `/Applications/Hutan.app`, `~/Library/Application Support/Hutan`, `~/.ssh`, harness homes or the Keychain; no model call was made.

| Piece | What it was |
|---|---|
| Daemon | `hutand 0.2.0` **release** build copied from the main checkout's `target/release` (protocol **0.42**; the repo is at 0.46). `HUTAN_HOME=~/hutan-scratch/remote-spike/h`. 0.43–0.46 add memory/usage/terminal-search methods, not transport behaviour [INFERENCE]. |
| SSH | A throwaway user-level `sshd` (`/usr/sbin/sshd -f …`, OpenSSH 10.3p1) on `127.0.0.1:2222`, its own host key and Ed25519 client key, `AllowStreamLocalForwarding yes`, `ssh -F none … UserKnownHostsFile=<scratch>`. Real handshake, encryption and channel flow control; **loopback**, so there is no network RTT, bastion or NAT. |
| TLS | `tlsb`, ~80 lines of Rust (`rustls 0.23.46`, `ring` provider — the workspace's choice, see `Cargo.toml:75-85` — TLS 1.3 only, **mutual** auth, P-256 CA, server and device certificates) relaying raw bytes between a TCP socket and the daemon's Unix socket. It polls with a 50 µs sleep, so its numbers are an **upper bound**. No WebSocket framing was prototyped (no WS crate is in `Cargo.lock`). |
| Client | A ~150-line Python harness speaking the real frames (`u32 BE length | u8 kind | payload`), `hello`, JSON requests, `terminal.attach`/`terminal.ack` (the render frame number is the first postcard varint after the enum tag; the harness never decodes screens). Python itself costs ~20 µs per request (direct p50 0.022 ms). |
| WAN | A Python asyncio relay adding a fixed one-way delay per direction in front of each transport. It costs a **constant ~4.7 ms** (RTT 100 ms measures 104.7). No bandwidth cap, no loss (no `dnctl`/`pfctl`: needs root). |

Pitfalls found while prototyping — each one will bite the implementation:

- macOS's system Python 3.9 is linked to LibreSSL 3.3.6 and cannot speak TLS 1.3 (`ValueError: Unsupported protocol version 0x304`). Anything that drives TLS tests on the dev machines must use rustls.
- LibreSSL's `openssl x509 -req` signs with **ecdsa-with-SHA1** by default; rustls then refuses the certificate (`UnsupportedSignatureAlgorithm`). `openssl ecparam` writes *explicit* curve parameters that rustls cannot parse as a key (`failed to parse private key as RSA, ECDSA, or EdDSA`). Pairing must generate certificates in-process (`ring`/`rcgen`), never by shelling out to `openssl`.
- `ssh -L <local.sock>:<remote.sock>` fails when a previous run left `local.sock` behind: `unix_listener: cannot bind to path …: Address already in use / Could not request local forwarding`, exit 255. With `-o StreamLocalBindUnlink=yes` the same command replaced the stale socket and the next `hello` succeeded. Any managed forward must set it (and `ExitOnForwardFailure=yes`).
- A malformed epoch in `hello.resume` is `bad_request: invalid length`, not `snapshot_required`: a client must send the epoch it was given (26-character ULID).

## 1. Use cases, ranked (brief question 1)

Evidence is the user-needs spike (`2026-10-25-user-needs/{agents-cli,editors,terminals,multi-agent,ade}.md`) plus the two Codex issues read in full for this spike.

| Rank | Use case | Evidence | What the people asked for |
|---|---|---|---|
| **1** | **Drive a daemon on another Mac / Linux box / devcontainer from this desktop** | Zed #11473 (930 reactions, 163 comments, closed): dev containers and remote servers. Codex #10450 "Remote Development in Codex Desktop App" (878): SSH remote hosts, remote filesystem as the single source of truth, several hosts, "keep running after I close the app", hosts from `~/.ssh/config`, `WorkspaceName@SSHHostName`. Warp #6744/#6831 (79/72), cmux #1664 (38), Superset #1513 (34, closed duplicate), AO #3853. | The Codex thread's own answers: the remote workspace is the truth (no sync), several machines as *projects*, work survives closing the app, "do not keep punching holes" (port forwarding). Hutan already has the survival half: the daemon owns processes (ADR 0001, 0009). |
| **2** | **Watch and approve agents from a phone** | Codex #9224 "Codex Remote Control" (544): the author already does it with Tailscale + an SSH client and wants "a proper mobile app UI"; comments ask for approval and continue-the-conversation, and warn that remote shell access is "a giant backdoor" and relays should be end-to-end encrypted. Orca #3099 (28, 12 comments), C#89 (6). Community stopgaps in that thread: a Telegram bridge that forwards approval prompts (pikiclaw), web clients with outbound-only connections (zane, codex-pocket, RemCodex). Claude Code shipped Remote Control (below). | Unblock an agent away from the desk; see "needs you" first; reply; stop. Not a remote terminal. |
| **3** | **Share a session read-only** | No issue in the cited set asks for it directly. It falls out of the phone work: an `observe`-scoped device limited to one session with an expiry. [INFERENCE: demand] | Show a teammate or a second screen what an agent is doing without giving control. |

The brief lists the phone first; the reaction counts put the desktop-to-remote-host case first by roughly 2 : 1, and its transport (SSH) is also the cheapest and the safest. The phone case has the bigger *design* surface (auth, scopes, notifications) and gets most of this document.

Competitor reference (verified): Claude Code Remote Control makes **outbound HTTPS only**, "never opens inbound ports", registers with the Anthropic API and routes messages through it; the session transcript is stored on Anthropic's servers; the account must be a Pro/Max/Team/Enterprise subscription; an organization can require **Trusted Devices** — each phone/browser enrolls its own key, with a recent sign-in or a Face ID/passkey step-up ([Remote Control](https://code.claude.com/docs/en/remote-control)). That is the relay model; it needs a vendor service and only drives Claude Code. Hutan's differentiator is one attention list across all harnesses (Codex, Claude Code, OMP), with the daemon authoritative on the user's own machine.

## 2. Transport options (brief question 2)

### 2.1 Measured: the existing protocol over each path

Real `hello` / `daemon.status` / `session.list` through each path, 500 samples after 20 warm-ups, milliseconds, loopback (no network RTT):

| Path | `daemon.status` p50 / p95 / p99 | `session.list` p50 / p95 | Cold connect + `hello` |
|---|---|---|---|
| A. direct Unix socket (today) | 0.022 / 0.032 / 0.048 | 0.020 / 0.031 | 0.1–0.2 |
| B. `ssh -N -L local.sock:remote.sock` (stream-local) | 0.087 / 0.161 / 0.212 | 0.080 / 0.114 | 0.3–0.6 on a **warm** forward |
| C. `ssh -N -R remote.sock:local.sock` (reverse stream-local) | 0.083 / 0.114 / 0.132 | 0.080 / 0.112 | 0.3–0.4 |
| D. `ssh host nc -U sock` (stdio, no PTY; stand-in for `hutan proxy`) | 0.113 / 0.182 / 0.203 | 0.094 / 0.146 | **92.7–99.7** (SSH handshake + exec) |
| E. mutual TLS 1.3 over TCP → Unix socket (spike relay, upper bound) | 0.264 / 0.367 / 0.425 | 0.302 / 0.395 | 0.7–1.1 (TLS handshake, loopback) |

- SSH's own cost is small: B/C/D add 60–90 µs at p50. The **connection** cost dominates: ~95 ms cold (`ssh true`: 0.09 s ×3), **0.04 s ×3 through a ControlMaster** — keep one persistent connection per host, as the D-3 spike recommends (one framed connection already multiplexes everything).
- A forwarded socket is accepted by the daemon's peer-UID check (`conn.rs:304`) because the forwarding `ssh`/`sshd` child runs as the same user: **SSH authentication is the only authentication in this path**, so the host key and `authorized_keys` policy are the trust root. `-R` works the same way and exists for hosts that can only dial out (devcontainer → laptop), but then any process of that user on the far side reaches the socket.
- Stdio via `nc -U` works here because macOS ships `nc -U`; minimal Linux images may not (BusyBox `nc` lacks `-U` [INFERENCE]). A ~100-line `hutan proxy` (bytes only, flush per read — the D-3 spike's buffering pitfall) removes the dependency.

### 2.2 Measured: round trips and terminal output at WAN latency

The delay relay sat in front of an `ssh -L` forward and, separately, of the TLS path. Request and echo figures are given for both (ssh / TLS; they agree within ~2 ms); the flood columns were measured through the `ssh -L` forward:

| RTT (emulated) | `daemon.status` p50 | `hello` | Echo of one typed byte → next render (idle terminal, p50) | Terminal flood: renders/s | Flood downstream | Flood upstream (acks) |
|---|---|---|---|---|---|---|
| 0 ms | 0.30 (ssh) / 0.43 (TLS) | 1.1 / 1.3 | 0.63 / 0.70 | 93.4 | 53.6 kB/s | 8.4 kB/s |
| 50 ms | 54.7 / 52.9 | 55.1 / 51.3 | 54.8 / 53.2 | 18.4 | 10.5 kB/s | 1.7 kB/s |
| 100 ms | 104.7 / 102.9 | 105.1 / 103.8 | 104.8 / 103.2 | 9.6 | 5.5 kB/s | 0.9 kB/s |
| 200 ms | 204.7 / 202.9 | 203.5 / 205.3 | 204.8 / 202.9 | 4.9 | 2.7 kB/s | 0.5 kB/s |

(Flood = `while :; do seq 1 20000; done` in a 120×40 terminal, acking every render, 6 s window; inter-render gap p50 = 10.7 / 54.7 / 104.8 / 204.8 ms.) Reading:

- **Request/response and echo cost one RTT plus the transport overhead of §2.1** (the 50–200 ms rows also contain the emulator's constant ~4.7 ms, so they read RTT + ~5 ms; at 0 ms the whole cost is 0.3–0.7 ms): the existing protocol has no chatty handshake — `hello` is one round trip, and a cold TLS connection adds TCP + TLS 1.3 handshakes (2 RTT) on top, a cold SSH connection ~3 RTT [INFERENCE from the protocols; loopback measured 0.7–1.1 ms and 93–100 ms respectively].
- An earlier combined run reported lower flood rates (6.2 renders/s at 100 ms) because its harness shared the connection with a just-closed echo terminal and acked frames of the wrong terminal; it was discarded. The isolated run above is the reported one, and the inter-render gap on three separate paths at 100 ms RTT agrees (direct 104.5, `ssh -L` 104.4, TLS 103.0 ms p50).
- **Credit-1 pacing (ADR 0004) behaves as designed**: one render per RTT plus the daemon's ~8–10 ms floor. Under a flood a 200 ms link shows ~5 screen updates a second — fine for *watching* an agent, noticeably steppy for *typing into* a busy full-screen program. The daemon coalesces (latest state wins), so nothing queues.
- **Bandwidth is irrelevant**: even the worst-case flood is 54 kB/s at LAN speed and 2.7 kB/s at 200 ms. An initial snapshot of a 100×40 screen is 2 728 bytes; `session.snapshot` with a few sessions 1 345 bytes; 50 history rows 3 929 bytes of JSON. A phone link is never the bottleneck; **latency, radio sleep and half-open connections are**.
- A typed byte *while a render is unacknowledged* waits for the ack cycle (≤ 2 × RTT + 20 ms, the D-3 spike's target) — not measured separately here because a flood terminal does not echo input legibly [INFERENCE].

### 2.3 Measured: slow clients and reconnects over a remote path

- **Slow consumer.** A client that subscribes to events and stops reading while 6 000 `session.rename` events are generated: directly on the Unix socket the daemon closes it (`WARN slow_consumer: outbound queue full, closing connection … queue_capacity=1024`, the ADR 0002 behaviour). Through `ssh -L` the reader got **exactly 2 063 kB, then EOF** for 6 000, 20 000 and 60 000 events: OpenSSH absorbed ~2 MiB (matches its default channel window [INFERENCE]) before the daemon's queue filled, and then the daemon closed the connection as designed. **The queue bound is by message count; the effective memory behind a remote client is the message bound plus the transport's buffers** — one more reason the D-3 spike proposes byte accounting (§2.6).
- **Sleep and wake.** Subscribe through `ssh -L`, drop the connection, create a session and rename it 20 times, reconnect with `hello.resume {epoch, seq}`: welcome says `resume: "replayed"` in 0.6 ms and 23 control frames (`session.updated`, `workspace.updated`) arrive immediately; a different epoch, or a `seq` older than the ring, answers `snapshot_required`. ADR 0002's replay-or-snapshot is exactly the recovery a phone that was suspended needs; **no protocol work** is required for reconnect.

### 2.4 Options compared

| | **SSH stream-local forward** (`-L`/`-R`) | **SSH stdio** (`ssh host hutan proxy`) | **TLS / WebSocket listener + device pairing** | **Tailscale-only listener** | **Relay service** |
|---|---|---|---|---|---|
| Hutan code | none (docs + a managed `ssh -N` supervisor) | `hutan proxy` + endpoint type (R-1/2) | rustls listener, WS framing, pairing, device registry | the same web listener, bound to loopback, **plus nothing of Tailscale** | broker service, tickets, device/host keys, reconnect logic |
| New listener on the host | none (sshd) | none (sshd) | **yes** (TCP port) | loopback only; `tailscale serve` exposes it to the tailnet | none on host (outbound); **broker is a new public service** |
| NAT / mobile reach | needs SSH reachability (jump host, VPN) | same | needs reachability (LAN/VPN/port forward) | tailnet handles it (WireGuard NAT traversal) | **best**: outbound-only on both ends |
| Works from a phone browser | no (SSH clients cannot speak the frames; Termius-style app attaches a *shell*, not Hutan) | no | **yes** (WSS) — needs a CA-trusted certificate for a PWA | **yes**; `tailscale serve` provisions HTTPS ([docs](https://tailscale.com/kb/1312/serve)) | yes |
| Auth | SSH keys/agent/ProxyJump/host-key policy of the user's own `ssh_config` | same | per-device key; invite QR; revocation list | tailnet identity + ACL **and** Hutan device key (identity headers are spoofable by any local process if the backend is reachable off-loopback — Tailscale's own tip) | account + device key + inner E2EE |
| Measured here | +60–90 µs p50; cold 95 ms / 40 ms mux | +90 µs; cold ~95 ms | +0.24 ms (spike relay, upper bound); cold 0.7–1.1 ms loopback | not run (no Tailscale on this machine) | not built |
| Operational cost | none | none | cert issuance/rotation, firewall | needs a Tailscale account on both ends | **someone runs it**, plus metadata exposure; abuse handling |
| Failure modes | stale forward socket (§0), `ExitOnForwardFailure`, `nc` flavour | stdout pollution by `.bashrc` banners (D-3 §5), proxy missing on host | key custody, self-signed trust on iOS, DNS rebinding of loopback listeners | Funnel is public (no identity headers); serve DNS names fixed | transcripts on the broker unless E2EE (Claude RC stores them) |
| Verdict | **desktop → host, now** | **desktop → host, default** | native/desktop-to-desktop later; web needs it anyway behind a front | **phone path, recommended front** | **defer**; own ADR |

The prior spike's "SSH first → TLS second → relay later" stays; this spike adds that the **phone path is not the TLS listener of a native client but a loopback web listener**, because a phone has no SSH-forward story and a PWA needs a certificate a browser trusts. A self-signed certificate is acceptable to the native desktop (it pins) but not to an iPhone web app: service workers and Web Push require a secure context ([Service Worker API](https://developer.mozilla.org/en-US/docs/Web/API/Service_Worker_API), [Push API](https://developer.mozilla.org/en-US/docs/Web/API/Push_API) — an active service worker is mandatory) and a certificate warning blocks installation [INFERENCE: not tested on a device]. A valid name comes for free from `tailscale serve` (`<machine>.<tailnet>.ts.net`, Let's Encrypt) or from the user's own reverse proxy; Hutan should not become a certificate authority for browsers.

### 2.5 Auth, pairing and what is exposed

**Principals.** Today a connection is the *user* (peer UID) or *agent-originated* (origin session, ADR 0016). A remote device is a third thing: the user, from a device that can be lost. It must not inherit "the user".

**Pairing (proposed).**
1. `hutan remote pair [--scope approve] [--session ID --expires 1h]` (desktop: Settings → Remote) prints a QR and URL `https://<host>/pair#<invite>`; the invite is 128 random bits in the URL **fragment** (never sent to a server or log), valid 5 minutes, single use, hashed in the daemon.
2. The phone generates a **non-extractable P-256 key** (WebCrypto; native clients: Secure Enclave) and sends `{invite, public_key, name}`. The desktop shows "Pair *iPhone 17*? code 482 913" computed from both sides (a short authentication string); the user confirms on the desktop. This is the step that defeats a photographed QR.
3. The daemon stores the device **public key**, name, scope, session allowlist, `created/last_seen/expires`, `revoked` in SQLite — no secret. Each connection proves possession with a signed challenge **before** `hello` is processed (the D-3 spike's rule: authentication happens in the acceptor, never in a JSON field).
4. `hutan remote devices ls|revoke`; a revoke closes that device's connections and rejects resume.

**Where keys live (ADR 0008, "secrets only in Keychain via the daemon").**

| Key | Where | Why |
|---|---|---|
| Device key of a phone | the phone (WebCrypto non-extractable key in the web app's storage; Secure Enclave for native) | never leaves the device; the daemon only holds the public half. iOS may evict web-app storage [INFERENCE] → the phone re-pairs. |
| Listener TLS key (direct-TLS mode, RM later) and the **VAPID private key**, **Web Push subscriptions** | macOS Keychain via the daemon (Secret Service on Linux; fail closed if unavailable) | a push subscription's endpoint URL is a capability: "knowledge of the endpoint is all that is necessary to send a message" ([MDN](https://developer.mozilla.org/en-US/docs/Web/API/Push_API)), so it is a secret, not a SQLite column. |
| The *desktop's* credential to a remote daemon (direct-TLS mode) | proposal: the **local daemon** keeps the device key and signs challenges (`remote.sign`); the desktop never reads it | keeps ADR 0008 literal (clients never hold secrets). SSH needs none: the user's `ssh-agent` and keys. |

**What a device may do — a scope ceiling beside the control policy.** `[control] agents` gates *agent-originated* mutations (ADR 0016, `docs/control.md`); it does not describe a *device*. Add a fourth classification to the one exhaustive table in `router.rs` (the compiler already forces a decision for every new request):

| Scope | Allowed | Notes |
|---|---|---|
| `observe` | every **Read** request except the file and editor services (`fs.*`, `doc.*`, `lsp.*`), `history.search` and `terminal.search`; events; `terminal.attach`/`history`/`block_text` for allowed sessions; chat journals | A shared session limits this to a session allowlist. Terminal input frames (kind 2) from an observe device are dropped and counted. |
| `approve` | `observe` + `approval.respond`, `notify.ack|act|mute`, `question.answer`, `chat.interrupt`, `agent.interrupt`, `session.stop` | These are the phone's four jobs: attention list, approvals, chat view, **stop**. Today `approval.respond` and `notify.act` are UserOnly *for agents*; for devices they are the whole point. |
| `operate` | `approve` + the remaining **Mutate** requests: `chat.send` / queue, `session.create`, `session.send_text` and terminal input, tasks, worktrees, `ui.run` | The device is the user; the existing Mawas/agent rules are unchanged. `daemon.shutdown`, `worktree.finish`, `history.clear` stay `local`. |
| `local` (default for any request nobody classified) | everything else, including every **UserOnly** request that is not listed above | Never reachable remotely in v1: provider credentials (`provider.*`, ADR 0008), account login/logout (ADR 0015), `daemon.settings.set` and so `[control]` itself, `control.grants.*`, `memory.*` writes, `lsp.trust`, `project.git_init`, device management. A phone can never widen its own scope, and **no secret ever travels inward**; secrets never travel outward either, because no request returns one. |

Audit follows the existing rows: `approvals.decided_by` already records the client kind (`desktop`, `cli`, `notifier`, protocol 0.15); add `remote` plus the device id, and a `hutan::audit` line per device mutation. A device that lost its scope for a request answers `forbidden` with `error.data.remote.scope_required`.

**What this does *not* solve** (say it in the UI): an `approve` device can approve a destructive tool call the agent asks for. The Mawas "what always asks" list (kill, delete, send_text) keeps requiring a deliberate tap; a **passkey/biometric step-up** for those, as Claude's Trusted Devices does, needs a real hostname (WebAuthn is bound to a domain, not an IP [INFERENCE]) — available behind `tailscale serve`, not on a bare LAN address. Device compromise equals the scope it was granted; vaults do not change that (D-3 spike risk table).

**Browser-specific attacks on a loopback listener.** DNS rebinding and cross-site requests can reach `127.0.0.1` from any page open on the Mac. The listener must check `Host` against a configured allowlist (the `ts.net` name, `localhost` only when explicitly enabled), check `Origin` on the WebSocket upgrade, and **never accept a cookie or a bearer token alone**: the device-key challenge is per connection.

### 2.6 Backpressure over a WAN (bounded queues)

- Terminal output needs nothing new: credit-1 is already RTT-paced and coalescing (§2.2).
- **Control queue.** 1 024 *messages* is a bound on count, not memory; a 4 MiB frame is legal. Add a per-remote-connection **byte** budget (D-3 proposes 8 MiB, "not a measured optimum") that counts what is queued and what is being written; the connection that exceeds it is closed `slow_consumer` and recovers by replay (measured, §2.3). Count the transport's own buffering in the budget: ssh holds ~2 MiB before the daemon sees pressure.
- **Idle and half-open.** A phone that sleeps leaves a TCP connection the daemon cannot distinguish from a slow reader; with few events the 1 024-message bound is not reached for a long time. A remote listener needs **WebSocket ping every 20 s, close after 60 s without a pong** [proposal, not measured], and a per-device cap on concurrent connections.
- **Delta flood on a chat.** A streaming turn emits many small `chat.event`s; at WAN speed the count bound, not the bandwidth, is what trips. Measuring that needs a real harness turn (not run: no model call). Proposal: a capability-gated `events.coalesce` for remote devices (merge consecutive deltas of one message) [INFERENCE: unmeasured].
- **Bulk responses.** `terminal.history` (≤ 5 000 rows / 2 MiB) and `session.snapshot` are single frames; paginate on mobile (50 rows = 3.9 kB).

## 3. Mobile surface (brief question 3)

### 3.1 Web app served by the daemon vs native iOS

| | **Web app (PWA) from the daemon** | **Native iOS app** |
|---|---|---|
| Install | scan the pairing QR → "Add to Home Screen" | App Store / TestFlight; **Apple Developer Program** membership [INFERENCE: required to ship and to use APNs; fee not verified here] |
| Wire | JSON control frames over WebSocket (frames carried as binary messages); the mobile subset needs no postcard decoder because it never attaches terminals | same frames natively; Rust client could share `hutan-client`/`protocol` via UniFFI [INFERENCE] |
| Push | **Web Push**, no developer account, from the daemon (§3.3) | APNs, which needs the **publisher's** APNs key, i.e. a Hutan-operated push server |
| Background | the page is suspended; reconnect + `resume` (§2.3) | same, with background modes limited |
| Auth keys | WebCrypto non-extractable key; passkeys possible on a real hostname | Secure Enclave, Keychain |
| Cost | a few hundred lines of static HTML/JS embedded in `hutand` + a listener | a second app, release pipeline, review |
| Reaches Android/desktop browsers | yes | no |

**Decision: web app first**, in the daemon, behind the loopback listener. A native client is justified only by (a) App-Store-level reliability of background notifications, or (b) a need to attach full terminals on the phone. Neither is a v1 requirement.

### 3.2 Minimal surface (v1)

Everything below exists today as JSON requests/events (`docs/protocol.md`); no render frames:

| View | Source |
|---|---|
| **Attention list** ("needs you" first: approvals, blocked agents, questions, failures, finished) | `session.snapshot`, `events.subscribe`, `approval.list`, `notify.list` / event `notify.intent` |
| **Approvals** with the provider's own option labels | `approval.required` → `approval.respond {approval_id, option_id}` (`decided_by` records the device) |
| **Chat view** (journal, streaming reply, send a message, queue) | `chat.*` journal and `chat.event`; `chat.send`, `chat.queue.*` need `operate` |
| **Terminal tail** (last output blocks; no live screen) | `terminal.blocks`, `terminal.block_text`, `terminal.history` (JSON) |
| **Stop** | `chat.interrupt`, `agent.interrupt`, `session.stop` |
| **Share (read-only)** | a device with `observe` and a session allowlist |

Out of v1: live terminal attach (needs a postcard decoder or a daemon-side "text mirror"), files and editor, worktrees/Finish, settings, Mawas administration. The live terminal on a phone is the case where the native client or an SSH app (Termius/Blink running `hutan attach`) is still the better tool.

### 3.3 Getting notifications to the phone

| Route | Needs | Verified facts | Fit |
|---|---|---|---|
| **ntfy** (ntfy.sh or self-hosted) | an HTTP POST from a USER-HOOKS hook on `approval.requested`/`turn.finished`; the ntfy iOS app | publish is a plain `PUT/POST`; headers `Title`, `Priority`, `Tags`, `Click` (open a URL on tap), `Actions` (`http` buttons) ([publish docs](https://docs.ntfy.sh/publish/)); "the topic is essentially a password" (use a 128-bit random topic or access tokens); a self-hosted server must forward poll requests to ntfy.sh for instant iOS delivery ([config](https://docs.ntfy.sh/config)) | **Stopgap, works the day USER-HOOKS lands**: alert with the session title and a `Click` URL into the PWA. An `http` action button could call a Hutan endpoint, but that needs the web listener and an `approve` device token in the header — do not ship that before R-4. |
| **Pushover** | an API token + user key; paid app | emergency priority `2` with `retry`/`expire` repeats until acknowledged ([API](https://pushover.net/api)) — the closest match to "an approval is waiting" | Optional second stopgap; same hook, same payload rules. |
| **Web Push from the daemon** | a web app on the Home Screen (iOS ≥ 16.4), VAPID keypair, the subscription from the phone | "Web Push on iOS and iPadOS uses the same Apple Push Notification service … You do not need to be a member of the Apple Developer Program"; permission must follow a user tap; allow `*.push.apple.com` egress ([WebKit](https://webkit.org/blog/13878/web-push-for-web-apps-on-ios-and-ipados)); Badging API supported | **Target.** The daemon is the application server: it signs with VAPID and POSTs encrypted payloads to the subscription endpoint (RFC 8030/8291/8292 — implementation crates not evaluated here [INFERENCE]). No Hutan server, works while the Mac is on. Payload carries only an id and a generic title; the PWA fetches detail over the paired connection. |
| **APNs (native app)** | developer account, signing key, a server holding it | requests to APNs are authenticated with the provider's own credentials ([Apple](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns)) | Only with a Hutan-operated service; defer. |

Payload rule for every third-party hop (ntfy.sh, Pushover, Apple): reuse the notification intent's already-redacted, bounded `title` (≤ 120) and `body` (≤ 240) (`docs/protocol.md` § Notification intents), or less (kind + session title). Never transcript text, paths with usernames or diffs. The existing `[notify]` gate (kinds, mute, focus suppression, 90 s burst limit) applies before any sink.

A caveat for **remote hosts**: `hutand` posts native macOS notifications itself, only from the signed bundle (ADR 0007). A Linux or remote daemon posts none, and a click while the poster is not running is lost. So for use case 1 the *local* desktop (or the local daemon acting as a watcher of its remote hosts) must post notifications for remote intents; that is R-3's job, not the phone's.

## 4. Linux daemon feasibility (brief question 4)

The Linux and Windows compile inventory is `docs/spikes/2026-10-18-cross-platform.md` (all 11 headless packages pass `cargo check --target aarch64-unknown-linux-gnu`; the Docker leg could not run; no native Linux test was executed). This table re-checks each crate against the current tree for the *remote host* role only (no GUI):

| Crate | macOS-only code? (evidence) | Linux work | Effort [INFERENCE] |
|---|---|---|---|
| `core`, `protocol`, `buffer`, `syntax`, `lsp_client`, `chat_view`, `agents` | none; `agents` spawns harness processes over stdio; `lsp_client` uses `rustix` | CI lane only | 0 |
| `storage`, `index` | none (bundled SQLite needs a C compiler; `index/src/sys.rs` already has the non-macOS `setpriority` branch) | Linux CI, tests | 0.5 |
| `settings` | `~/Library/Application Support/Hutan` default (`store.rs:43-64`); `0700` directory via `std::os::unix` (fine on Linux) | XDG paths | 1 (`hutan-platform` path resolver, XP-1a) |
| `client` | socket default `~/Library/…/hutand.sock` (`socket.rs:4-28`); `UnixStream` concrete (`client.rs:68-69`) | resolver; generic stream = R-1 | in R-1 / 1 |
| `daemon` | `paths.rs:64-77` macOS paths; `notify_native` and `menu_bar` are `cfg(target_os = "macos")` with non-macOS stubs (`notify_native.rs:41-95`); `objc2*` are target-gated dependencies (`crates/daemon/Cargo.toml`); `harness.rs:465-475` already lists `/usr/local/bin` and nix | paths, optional systemd user unit, tests | 1–2 |
| `terminal` | `procs.rs:334-343` returns **empty** children/inspect on non-macOS (no process trees, CPU/RSS, `hutan ps|top`, resource-aware kill); `env.rs` probes `/usr/libexec/path_helper`, `/bin/launchctl`, `/usr/bin/defaults` (`env.rs:48,215,226`); `sys.rs:144-165` `user_temp_dir` is `None` off macOS. PTY (`portable-pty`), `alacritty_terminal`, `rustix` are portable. | `/proc` scanning; Linux login-environment strategy (no launchctl) | 3–5 |
| `gateway` | `credentials.rs:68-100`: non-macOS gets `UnsupportedStore` — `get` returns none, `set` errors "needs the macOS Keychain" | Secret Service store; headless box without a keyring daemon must **fail closed** (ADR 0008), pointing at harness logins (ADR 0015: tokens live in the harness's own home on that host, never forwarded). `oo7 0.6.0` is already in `Cargo.lock` via the GPUI Linux backend [INFERENCE: usable standalone] | 3–5 |
| `cli` | passes `cargo check` on Linux; Windows is the problem | none | 0 |
| `desktop`, `terminal_view`, `code_view` | macOS-only today (objc2 dependencies, `open`); GPUI Linux features not enabled | **not needed for a Linux host**; Linux *desktop* is XP-2 | n/a |

**Total for a headless Linux `hutand` ≈ 8–13 engineer-days [INFERENCE]**, the same envelope as XP-1a/1b/1c of the cross-platform spike. Mac-to-Mac remote needs **none** of it, which is why LX-1 runs in parallel and is not on the critical path of the first remote slice.

Other host prerequisites the code does not provide: (a) the harnesses (`omp`, `claude`, `codex`) must be installed **and logged in on the host** (their own login flow runs in a terminal session there); (b) the host needs `git`; (c) a **devcontainer** is a Linux host whose `hutand` outlives the container only if its data directory is a volume [INFERENCE: Docker unavailable here, not tested]; (d) installing: `hutan remote install user@host` copying a pinned, checksum-verified binary (D-3 RM-4) — start with documented manual install.

## 5. Recommended path (brief question 5)

### 5.1 Slices

Estimates are [INFERENCE], engineer-days including focused tests. Numbers follow the D-3 and cross-platform spikes where those already define the slice. R-1, R-2 and R-3 have the scope of D-3's RM-1, RM-2 and RM-3 (acceptance tests: `2026-10-18-remote.md` § 18); R-4 onwards are new here (the D-3 spike's RM-4…RM-8 — install, files/uploads, TLS pairing, WAN scheduling, relay — keep their names and are not scheduled by this document, except that R-9 absorbs D-3's RM-7).

| # | Slice | Delivers | Protocol impact | Depends on | Days |
|---|---|---|---|---|---|
| 1 | **R-1 TRANSPORT-SEAM** | Generic async stream in `client` and `daemon::conn`; peer-UID check stays in the local acceptor, authentication produces an `AuthenticatedPeer`; typed endpoints, local-only autostart | none | — | 3–5 |
| 2 | **R-2 SSH-HOSTS** | `hutan proxy` (bytes only, flush per read, no PTY), supervised system `ssh`, `[[host]]` profiles, `hutan --host NAME …`; managed `-L` forward with `StreamLocalBindUnlink` + `ExitOnForwardFailure` for third-party tools | none | R-1 | 4–6 |
| 3 | **LX-1 LINUX-HEADLESS** (parallel) | §4: XDG paths, `/proc` process tree, login env, Secret Service, Linux CI; install doc | none | — | 8–13 |
| 4 | **R-3 HOSTS-DESKTOP** | `host.identity` (installation id), host-qualified desktop state, evidence-based status, no remote shutdown on window close, **remote intents → local native notifications** | minor, capability `host.identity` (+ optional `daemon.shutdown_receipt` from D-3) | R-2 | 8–12 |
| 5 | **NOTIFY-NTFY** (docs + recipe) | USER-HOOKS recipe for ntfy/Pushover with redacted payload; test with a local ntfy | none | USER-HOOKS | 0.5–1 |
| 6 | **R-4 DEVICE-AUTH** | device registry (public keys), scope column in `router.rs`, challenge-before-hello, revoke, audit `remote`, `forbidden` shape | minor, capabilities `remote.devices`, `remote.scopes` | R-1 | 5–8 |
| 7 | **R-5 WEB-LISTENER** | loopback-only HTTP/WebSocket listener (default off; `[remote.web]`), Host/Origin checks, WS ping/idle, per-device connection cap, `hutan remote pair|devices`, QR | none beyond R-4 (WS carries the same frames) | R-4 | 5–8 |
| 8 | **R-6 MOBILE-WEB** | the §3.2 PWA: attention list, approvals, chat, tail, stop; manifest + service worker | none | R-5 | 8–12 |
| 9 | **R-7 WEB-PUSH** | VAPID sender, subscriptions + key in Keychain/Secret Service, intents → push | minor `remote.push` (subscribe request) | R-6 | 4–6 |
| 10 | **R-8 SHARE-LINK** | session-scoped `observe` device with expiry, QR from the desktop | none | R-4 | 2–3 |
| 11 | **R-9 WAN-BUDGETS** | byte-accounted outbound queues, RTT fixtures (0/50/100/200 ms with loss), `events.coalesce` if the chat stream demands it | minor if `events.coalesce` | R-5 | 4–6 |
| — | Relay, native iOS, direct-TLS for native desktop clients | **not scheduled**; triggers in the ADR | — | separate security ADR | — |

**Order:** R-1 → R-2 → R-3 gives use case 1 on Macs; LX-1 adds Linux hosts and runs beside them; NOTIFY-NTFY gives the phone an alert as soon as USER-HOOKS lands; R-4 → R-5 → R-6 gives approve-from-phone; R-7 makes it push-driven; R-8/R-9 harden. R-4 is **security-critical and blocks every listener**: no non-local byte stream is accepted before challenge and scope enforcement exist.

### 5.2 Risks

| Risk | Mitigation |
|---|---|
| A network failure is read as "sessions ended" | D-3 evidence states (`live`, `unverifiable`, `refused`, `retired`); stale sessions stay visible |
| A lost phone controls agents that run arbitrary shell | scope ceiling (default `approve`), expiring pairings, instant revoke, biometric step-up for destructive approvals on real hostnames, honest UI copy; **default off** |
| Loopback listener abused by a web page (DNS rebinding/CSRF) | Host allowlist, Origin check, per-connection device-key challenge; no cookie auth |
| Tailscale identity headers trusted by mistake | use them for display only; the device key authorizes; listener binds loopback only (Tailscale's own advice) |
| Third-party push leaks content | minimal redacted payload, topic = 128-bit secret, user-chosen sinks, nothing leaves by default |
| iOS suspends the web app / evicts storage | `resume` replay (measured), re-pair flow, Web Push wakes via the service worker |
| Dead connections hold memory | byte budget, ping/idle timeout, device connection cap |
| Stale forwards and hostile shell startup files | `StreamLocalBindUnlink`, `ExitOnForwardFailure`, no PTY, `hutan proxy` validates its first frame |
| Remote host upgrade kills long jobs | explicit restart consent (D-3 RM-4); no hot handoff promised |
| Self-hosted relay becomes de-facto required for NAT | document `tailscale serve`; relay ADR when demand shows (Claude's relay is the benchmark: server-stored transcripts) |
| `hutand` growth: HTTP/WS stack in the authoritative daemon | feature-gated, off by default; `hyper 1.12`, `tokio-rustls 0.26.6`, `rustls 0.23.46` are already in `Cargo.lock`; a WebSocket crate and a Web Push crate are new (workspace deps, license check by `cargo-deny`) |

## 6. Questions for the user

1. **Order:** accept "remote host first, phone second" (evidence-ranked), or keep the brief's phone-first order? Slices reorder cheaply (R-4/5/6 do not depend on R-2/3).
2. **Phone front:** is "bring your own `tailscale serve`" acceptable as the only supported public exposure for v1, or must Hutan ship its own TLS listener (R-4-class work plus certificate custody)?
3. **Default scope** of a freshly paired phone: `approve` (recommended) or `observe`?
4. **Native notifications for remote hosts** (local daemon as watcher vs desktop-only): needs the user's call because ADR 0007 puts posting in the daemon.

## 7. Sources

- Evidence repo docs: `docs/spikes/2026-10-18-remote.md`, `2026-10-18-cross-platform.md`, `2026-10-25-user-needs/*`, `docs/protocol.md`, `docs/control.md`, ADR 0002, 0007, 0008, 0015, 0016; code: `crates/daemon/src/conn.rs:304`, `crates/gateway/src/credentials.rs:68-100`, `crates/terminal/src/{procs.rs:334-343,env.rs:48,215,226,sys.rs:144-165}`, `crates/daemon/src/notify_native.rs:41-95`, `crates/settings/src/store.rs:43-64`, `crates/client/src/socket.rs:4-28`, `Cargo.lock` (`hyper 1.12.0`, `rustls 0.23.46`, `tokio-rustls 0.26.6`, `oo7 0.6.0`; no `tokio-tungstenite`/`axum`/`web-push`).
- [Codex #10450 Remote Development in Codex Desktop App](https://github.com/openai/codex/issues/10450) (read in full for the maintainers' questions and users' answers) · [Codex #9224 Codex Remote Control](https://github.com/openai/codex/issues/9224) · [Claude Code Remote Control](https://code.claude.com/docs/en/remote-control) (connection and security, Trusted Devices).
- [WebKit: Web Push for Web Apps on iOS and iPadOS](https://webkit.org/blog/13878/web-push-for-web-apps-on-ios-and-ipados) · [MDN Push API](https://developer.mozilla.org/en-US/docs/Web/API/Push_API) · [Tailscale Serve](https://tailscale.com/kb/1312/serve) · [ntfy publishing](https://docs.ntfy.sh/publish/), [ntfy iOS/self-hosting](https://docs.ntfy.sh/config) · [Pushover API](https://pushover.net/api) · [Apple: sending notification requests to APNs](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns).
- [OpenSSH `ssh_config(5)`](https://man.openbsd.org/ssh_config.5) (`StreamLocalBindUnlink`, `ExitOnForwardFailure`, `ControlMaster`) · [`sshd_config(5)`](https://man.openbsd.org/sshd_config.5) (`AllowStreamLocalForwarding`) — behaviour confirmed by the prototypes above, text not re-read.

**Claims marked [INFERENCE]** are not verified here: the estimates in days; effects on iOS storage eviction, certificate trust and WebAuthn bound to hostnames; Apple's program fee; UniFFI sharing; `oo7` standalone; BusyBox `nc`; the ssh 2 MiB window explanation; devcontainer behaviour; chat-delta flood behaviour; crates for Web Push; unmeasured echo under credit exhaustion. **Not exercised:** Tailscale, a real network or phone, a Linux build or run, Docker, any Web Push round trip, any model call.
