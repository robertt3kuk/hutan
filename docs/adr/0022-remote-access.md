# 0022. Remote and mobile access: SSH for hosts, a paired loopback web app for phones

- Status: **proposed** (REMOTE-SPIKE, 2026-10-26; not accepted — see "Open for the user" below)
- Date: 2026-10-26
- Milestone: M9 (remote transport and mobile feasibility)
- Builds on: ADR 0001 (daemon authoritative), ADR 0002 (frames, replay, bounded queues), ADR 0004 (credit-1 terminal pacing), ADR 0007 (notifications posted by the daemon), ADR 0008 (secrets only in Keychain), ADR 0009 (process survival), ADR 0015 (logins stay in harnesses), ADR 0016 (one control surface, `[control]`)
- Extends: `docs/spikes/2026-10-18-remote.md` (D-3: SSH first → direct TLS → relay; transport seam, host-qualified UI, truthful status). That design stands; this ADR adds the phone path, device scopes, and the Linux prerequisite.
- Evidence: `docs/spikes/2026-10-26-remote/README.md` (measurements on the real `hutand`, option matrix, slices)

## Context
- Need #4 of the user-needs study: remote development from the desktop (Zed #11473 930, Codex #10450 878, Warp #6744/#6831, cmux #1664, Superset #1513) and phone control of agents (Codex #9224 544, Orca #3099). The daemon/client split exists; `docs/architecture.md` § Security says "no TCP listener until a remote transport ADR".
- The protocol runs over any reliable byte stream: replay-or-snapshot reconnect and credit-1 rendering already behave over a WAN. Measured through a real SSH forward: +60–90 µs per request, cold connection ~95 ms (40 ms multiplexed), reconnect with `hello.resume` replays in 0.6 ms; at emulated 50/100/200 ms RTT a request costs one RTT and terminal output is ack-paced to 18 / 9.6 / 4.9 renders/s using ≤ 10.5 kB/s.
- A phone cannot use an SSH forward, and a PWA needs a browser-trusted certificate for service workers and Web Push. Web Push on iOS (≥ 16.4, Home Screen app) needs no Apple developer account ([WebKit](https://webkit.org/blog/13878/web-push-for-web-apps-on-ios-and-ipados)); APNs for a native app needs a server holding the publisher's key.
- A remote shell-capable agent host is a high-value target ("a giant backdoor", Codex #9224): authentication and scope must be decided before any non-local byte stream is accepted.
- Mac-to-Mac needs no new daemon port; a Linux host needs the headless port (≈ 8–13 engineer-days [INFERENCE]; all 11 headless crates already pass `cargo check` for Linux).

## Options
| Option | Pros | Cons |
|---|---|---|
| **SSH (system OpenSSH): stdio `hutan proxy` / `-L` stream-local forward** | no listener; the user's `ssh_config`, agent, ProxyJump, host keys; measured +60–90 µs | cannot serve a phone; needs SSH reachability; stale forward sockets (`StreamLocalBindUnlink`) |
| TLS (rustls, mTLS) listener for native clients | works without SSH; measured 0.7–1.1 ms cold on loopback | exposed port, certificate and key custody, enrollment, revocation; self-signed is not usable by an iPhone web app |
| **Loopback HTTP/WS listener behind the user's `tailscale serve`, paired devices** | valid HTTPS and WireGuard from Tailscale; Hutan binds no external address; PWA + Web Push possible | needs a Tailscale (or equivalent reverse proxy) account; not run here |
| Relay service (outbound-only) | best NAT story; the Claude Code model | someone operates it; metadata/transcripts on a third party unless inner E2EE; separate security ADR |
| Native iOS app | App-Store reliability | developer program, a push server for APNs, a second client to maintain |

## Decision
1. **Transport boundary, no wire change.** `client` and `daemon::conn` run over any `AsyncRead + AsyncWrite`; the peer-UID check stays in the **local acceptor** and produces an authenticated principal; other acceptors produce theirs. Frames are never decoded or re-encoded by a transport (D-3 RM-1). Nothing outside an acceptor can construct an authenticated principal; a JSON `client.kind` is never authentication.
2. **Desktop → remote daemon uses system OpenSSH.** `hutan proxy` (bytes only, flushed per read, no PTY, exits on EOF) as the default; a supervised `ssh -N -L … -o StreamLocalBindUnlink=yes -o ExitOnForwardFailure=yes` for third-party clients. Hutan starts no listener for this. One persistent connection per host (ControlMaster optional). Local autostart never applies to a remote endpoint. Closing the desktop disconnects remote hosts; it never sends `daemon.shutdown` to them.
3. **Phone: a web app served by `hutand`**, behind a **loopback-only** HTTP/WebSocket listener, **off by default** (`[remote.web] enabled = false`, `listen = "127.0.0.1:<port>"`). Hutan binds **no** non-loopback address in this ADR; exposure to a tailnet or LAN is the user's reverse proxy (documented: `tailscale serve`). The listener checks `Host` against an allowlist and `Origin` on the WebSocket upgrade, ignores identity headers for authorization, and accepts **no cookie or bearer token alone**.
4. **Device pairing.** `hutan remote pair` creates a 128-bit single-use invite (5 min, hashed, carried in the URL fragment). The device sends a **public key**; the user confirms a short authentication code on the desktop. The daemon stores public key, name, scope, optional session allowlist, expiry and revocation in SQLite (no secret). Every connection proves key possession by a signed challenge **before** `hello`. `hutan remote devices ls|revoke`; revoking closes live connections and rejects resume.
5. **Scope ceiling per device**, enforced in the daemon as a fourth column of the one exhaustive classification table in `router.rs`:
   - `observe`: Read requests except `fs.*`, `doc.*`, `lsp.*`, `history.search`, `terminal.search`; terminal input frames dropped.
   - `approve`: `observe` + `approval.respond`, `notify.ack|act|mute`, `question.answer`, `chat.interrupt`, `agent.interrupt`, `session.stop`.
   - `operate`: `approve` + the other Mutate requests; never `daemon.shutdown`, `worktree.finish`, `history.clear`.
   - `local` (the default for anything unclassified): everything else, including every UserOnly request not listed — provider credentials and `account.login|logout` (ADR 0008/0015), `daemon.settings.set` (so `[control]`), `control.grants.*`, memory writes, `lsp.trust`, `project.git_init`, device management. **No secret crosses a remote transport in either direction.**
   `[control] agents` and Mawas's modes are unchanged; a device is the user, with a ceiling. A refused request answers `forbidden` with `error.data.remote.scope_required`. Audit rows record the device id and client kind `remote`. Default scope of a new device: `approve` [open question].
6. **Key custody (ADR 0008).** The daemon-side private material — VAPID key, Web Push subscriptions (their endpoint URLs are capability URLs), and in a later direct-TLS mode the listener key — lives in Keychain (macOS) / Secret Service (Linux), **failing closed** when unavailable. Device keys live on the devices. A desktop that talks to a remote daemon without SSH signs through the **local daemon**, never reading the key itself.
7. **Notifications to the phone.** Stopgap: USER-HOOKS recipes to ntfy/Pushover with the already-redacted intent title/body (≤ 120/240 characters; never transcript text). Target: **Web Push sent by the daemon** (VAPID) to the subscription of a paired PWA, subject to the existing `[notify]` gate; payload = kind + generic title + id, detail fetched over the paired connection. No Hutan-operated server, no APNs, no native app in this ADR. For remote *hosts*, the local desktop (or local daemon as watcher) posts native notifications for remote intents; a remote daemon posts none.
8. **WAN behaviour.** Credit-1 and coalescing are unchanged. Remote connections get a **byte-accounted** outbound budget in addition to the 1 024-message bound (default 8 MiB, to be measured), WebSocket ping 20 s / close at 60 s without pong, a per-device connection cap, and paginated bulk responses. Reconnect after sleep uses the existing `hello.resume`.
9. **Linux headless daemon** is the prerequisite for non-Mac hosts and proceeds as slice LX-1 (`docs/spikes/2026-10-18-cross-platform.md`): XDG paths, `/proc` process tree, Linux login environment, Secret Service credential store. It is not on the critical path of Mac-to-Mac.
10. **Not decided here:** a relay service, a native iOS app, direct TLS for native desktop clients (D-3 RM-6) and a relay (D-3 RM-8), Windows. Each needs its own ADR; triggers below.

## Protocol impact (additive; ADR 0002 rules)
| Change | Version |
|---|---|
| Transport seam, `hutan proxy`, SSH hosts, web listener carrying the same frames in WebSocket messages | none |
| `host.identity` (installation id on `Welcome`) | minor + capability |
| Device registry, pairing, scopes (`remote.devices`, `remote.scopes`; `forbidden` data shape) | minor + capabilities |
| Web Push subscription request (`remote.push`) | minor + capability |
| `events.coalesce` for remote devices (only if the chat stream measurement demands it) | minor + capability |
No change to frame kinds, lengths, replay or credit-1 semantics.

## Consequences
- Desktop → remote host ships with no daemon listener and no new secret; its security is the user's SSH configuration, stated in the UI.
- The router table gets a scope per request; adding a request without one is a compile error (as for Read/Mutate/UserOnly today).
- `hutand` gains an optional HTTP/WS stack (`hyper`, `tokio-rustls` are already in `Cargo.lock`; a WebSocket and a Web Push crate are new). Feature-gated and off by default so a headless host that never enables it carries no listener.
- The first phone slice (R-4) delivers nothing visible; it exists so no listener can land without authentication.
- A lost phone is bounded by its scope and revocation; an `approve` device can still approve a destructive request — the UI says so; step-up by passkey needs a real hostname.
- Users without Tailscale (or another valid-certificate front) get the ntfy/Pushover alert and SSH, not the PWA, until a relay or direct-TLS ADR exists.
- On acceptance, the line "no TCP listener until a remote transport ADR" in `docs/architecture.md` § Security becomes "loopback only, default off, authenticated devices only", and `docs/protocol.md` gains a § Remote devices; neither is edited by this proposal.
- Revisit when: (a) more than a small share of users ask for phone access without any VPN → relay ADR with inner E2EE; (b) native background reliability of Web Push proves inadequate → native iOS ADR; (c) a non-SSH desktop transport is requested → direct TLS (D-3 RM-6).

## Open for the user
Order of the two tracks (remote host first by evidence, phone first by the brief), whether `tailscale serve` is an acceptable sole public front for v1, default scope of a new device, and where native notifications for remote hosts are posted (desktop vs local daemon).

## Evidence
- `docs/spikes/2026-10-26-remote/README.md` §2.1–2.3: latency table (direct 0.022 ms; `ssh -L` 0.087; `ssh -R` 0.083; stdio 0.113; mTLS spike 0.264, p50 `daemon.status`), WAN table (RTT 0/50/100/200 ms), slow-consumer (ssh absorbs ~2 MiB before the daemon closes), resume (replayed 23 frames in 0.6 ms; wrong epoch or too-old seq → `snapshot_required`).
- Prototype pitfalls recorded there: LibreSSL signs with SHA-1 and writes explicit EC parameters (rustls refuses both) — generate certificates in-process; stale `-L` sockets need `StreamLocalBindUnlink=yes`.
- External: [Claude Code Remote Control](https://code.claude.com/docs/en/remote-control) (outbound-only relay, stored transcripts, Trusted Devices), [WebKit Web Push](https://webkit.org/blog/13878/web-push-for-web-apps-on-ios-and-ipados), [MDN Push API](https://developer.mozilla.org/en-US/docs/Web/API/Push_API) (subscription endpoint is a capability URL), [Tailscale Serve](https://tailscale.com/kb/1312/serve) (HTTPS, identity headers, loopback advice), [ntfy](https://docs.ntfy.sh/publish/), [Pushover](https://pushover.net/api), [Codex #10450](https://github.com/openai/codex/issues/10450), [Codex #9224](https://github.com/openai/codex/issues/9224).
- [UNVERIFIED] Tailscale behaviour (not installed here), real-device PWA/Web Push, Linux runtime, Docker/devcontainers, all day estimates.
