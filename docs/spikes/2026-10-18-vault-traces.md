# Local secrets vault and local-first traces

**Status:** Research and proposed design for direction §8 / D-6. No implementation decision is implied.

## Executive recommendation

1. **Adopt a daemon-owned encrypted vault**, not a new password manager UI layered directly over OS credential stores.
   - Random vault root key.
   - Per-item data keys and XChaCha20-Poly1305 authenticated encryption.
   - OS credential store protects the root-key wrapping key.
   - Offer an explicitly stronger **OS store AND passphrase** mode, using Argon2id.
   - Support **passphrase-only** initialization for headless hosts without a usable OS store.
   - Never silently fall back to plaintext, an insecure sample backend, or an ephemeral store.

2. **Keep `secret://name` symbolic until an authorized daemon egress operation.** Resolve only while constructing a child-process environment, provider request header, or registered connector’s MCP arguments. Do not implement a general “get secret” MCP tool.

3. **Implement local traces as bounded structured metadata**, independent of OTLP availability. Use OpenTelemetry IDs, span relationships, and GenAI conventions, but keep Hutan’s storage model under Hutan’s control.

4. **Keep transcript content out of traces by default.** Reuse existing journal records through references rather than making another copy of prompts, reasoning, arguments, or tool output.

5. **Make OTLP export explicitly opt-in**, using the official `opentelemetry-otlp` crate. Export failure must not affect turns or local trace viewing.

**Important correction to the direction proposal:** adding a passphrase as another independent wrapper creates **OS store OR passphrase**, not additional protection against an unlocked OS account. The UI and ADR must distinguish recovery alternatives from a genuinely required second protection layer.

---

## 1. Scope, evidence, and existing behavior

This report follows the daemon-authoritative, local-first boundary in `docs/spikes/2026-10-18-direction.md:13-35,95-111`. The relevant prior findings are in:

- `docs/spikes/2026-10-18-hutan-model-lifecycle-transport.md:55-67,79-82`.
- `docs/spikes/2026-10-18-hermes-meta-harness.md:84-138`.
- `docs/spikes/2026-10-18-orca-process-remote.md:68`: encryption availability is an explicit outcome, not something to assume.
- `docs/spikes/2026-10-18-orca-session-history.md:460-470`: Orca’s transcript index can be another plaintext copy; that is not a precedent Hutan should adopt.

### Hutan today

| Area | Observed implementation |
|---|---|
| Provider keys | `CredentialStore::{get,set,delete}` returns a deliberately non-serializable `Secret`. Production uses macOS Keychain. Non-macOS reads return no key and writes fail; there is no production in-memory fallback. `crates/gateway/src/credentials.rs:28-36,68-99`. |
| Keychain identity | Generic-password service `dev.hutan.provider`, account = provider ID; database reference is `dev.hutan.provider/{provider_id}`. `crates/gateway/src/credentials.rs:15-21,168-199`. |
| Use-time loading | Provider resolution reads the credential store from a blocking task before constructing the adapter. `crates/daemon/src/providers.rs:427-439`. |
| Provider database pointer | Persistence writes `keychain_ref`, not the key. `crates/daemon/src/providers.rs:407-423`. |
| Secret memory handling | `Secret(String)` prevents accidental `Debug`/`Display`/serialization leakage, but is cloneable and has no zeroizing drop implementation in this file. `crates/gateway/src/secret.rs:1-23`. |
| Exact-key redaction | Gateway `Redactor` removes the credential a request used, plus recognizable key shapes. It currently only matches the exact credential when its length is at least four. `crates/gateway/src/redact.rs:14-46`. |
| Index redaction | Regex-based removal covers token shapes, authorization headers, PEM keys, URL passwords, and secret-looking assignments. Its own documentation explicitly calls this heuristic, not a guarantee. `crates/index/src/redact.rs:1-7,18-77`. |
| Restored environments | Secret-looking environment names are dropped from persisted terminal specifications, so restored sessions lose them. `crates/daemon/src/restore.rs:89-113`. |
| Chat event model | Events carry session, sequence, timestamp, optional turn/item IDs, provider IDs, typed event kind, and optional provider payload. Raw payload limit is 128 KiB; tool output limit is 256 KiB. `crates/core/src/agent.rs:40-60,89-110`. |
| Existing observability inputs | Turn start/end/failure/cancel, tool start/end, permissions, questions, usage and provider resume information already exist. `crates/core/src/agent.rs:283-429`. |
| Token semantics | Missing counts remain `None`, not zero. Input, cached input, output, reasoning output and total counts exist. Usage events distinguish cumulative conversation totals from latest-request counts. `crates/core/src/agent.rs:257-269,411-419`. |
| Journal | `chat_turns`, `chat_items`, and `approvals` already record durable history. `crates/storage/migrations/0003_chat.sql:4-47`. |
| Journal boundary | `journal_replacing` serializes the supplied `AgentEvent` directly; it is not itself a universal sanitization boundary. `crates/daemon/src/chat.rs:1845-1873`. |
| Partial snapshots | Open streaming items are periodically journaled, so sanitizing only final events would be insufficient. `crates/daemon/src/chat.rs:1416-1468`; `crates/core/src/agent.rs:301-331`. |

**Consequence:** “Hutan already redacts” is true for specific paths, but does not establish that every provider payload, partial snapshot, approval title, error, or future trace attribute is safe. The vault work needs a common boundary rather than another independently evolving regex list.

### Research limitations

This was source/document inspection. No Hutan binary, Keychain operation, paid model call, experiment, build, test, formatter, or dependency-policy check was run. Candidate-license conclusions below are not a claim that a future resolved dependency graph has passed `cargo deny`.

---

## 2. Candidate crates: ownership, licenses, maintenance, platforms

Hutan allows MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Zlib and MPL-2.0, among other explicitly listed licenses (`deny.toml:17-35`). Its workspace currently uses Rust 1.98 and bundled `rusqlite` (`Cargo.toml:5-10,59`).

### Comparison

| Candidate | License and maintenance evidence | Platform coverage / limitations | Recommended role |
|---|---|---|---|
| **`keyring` ecosystem** | MIT OR Apache-2.0. Current official manifest identifies `keyring` 4.2.0. The maintained project has split its API into `keyring-core` and platform-store crates. Official README recommends explicit stores for applications needing control over backends. [Repository](https://github.com/open-source-cooperative/keyring-rs), [manifest](https://raw.githubusercontent.com/open-source-cooperative/keyring-rs/master/Cargo.toml). | macOS Keychain/protected store, Windows Credential Manager, Linux/*nix Secret Service; additional backends exist. Availability and security depend on the selected backend and user session. | OS wrapping-key storage behind `hutan-platform`; prefer explicit adapters, not the all-stores CLI feature. |
| **`age` / `rage`** | Rust implementation: MIT OR Apache-2.0. `age` 0.12.1 is still described as beta; changelog records July 2026 fixes and a previously fixed plugin-execution vulnerability. Official age project links to `str4d/rage` as its interoperable Rust implementation. [age site](https://age-encryption.org/), [Rust repo](https://github.com/str4d/rage), [changelog](https://raw.githubusercontent.com/str4d/rage/main/age/CHANGELOG.md). | Rust library and binaries for macOS, Linux and Windows; file-oriented streaming encryption. Native password encryption uses **scrypt**, not Argon2id. Plugins can execute programs. | Portable encrypted export/backup, with plugins and SSH features disabled unless a separately approved requirement exists. Not the live item database. |
| **`chacha20poly1305`** | MIT OR Apache-2.0. Official RustCrypto repository contains algorithm-specific CI; docs report an NCC audit with no significant findings. That audit is historical, not an audit of Hutan or every current dependency. [Repository](https://github.com/RustCrypto/AEADs), [documentation](https://docs.rs/chacha20poly1305/latest/chacha20poly1305/). | Pure Rust; XChaCha uses a 24-byte nonce. Documented constant-time caveat on processors with variable-time multiplication. Appropriate target assumptions must be explicit. | **Preferred item and wrapping AEAD**, using full-round XChaCha20-Poly1305. |
| **`aes-gcm`** | MIT OR Apache-2.0; same RustCrypto maintenance/audit qualifications. [Documentation](https://docs.rs/aes-gcm/latest/aes_gcm/). | Pure Rust with hardware acceleration where available; default nonce is 12 bytes. Unique-nonce management needs particular care. | Valid alternative when interoperability or a defined compliance requirement demands AES-GCM. Merely using this crate does **not** establish FIPS certification. |
| **`argon2`** | MIT OR Apache-2.0. Official changelog records 0.6.0 on 2026-08-27, including allocation-failure handling and dependency/API updates. [Repository](https://github.com/RustCrypto/password-hashes/tree/master/argon2), [changelog](https://raw.githubusercontent.com/RustCrypto/password-hashes/master/argon2/CHANGELOG.md). | Pure Rust, Argon2id default; supports no-std configurations. Derivation consumes intentional memory/CPU and belongs off the async reactor. | Passphrase-to-wrapping-key derivation using `hash_password_into`, not a serialized password-verifier string. |
| **`secrecy`** | MIT OR Apache-2.0. Current documented 0.10.3; latest inspected changelog release is 2024-10-09. Low release frequency is observable; it is not by itself proof of abandonment. [Repository](https://github.com/iqlusioninc/crates/tree/main/secrecy), [changelog](https://raw.githubusercontent.com/iqlusioninc/crates/main/secrecy/CHANGELOG.md). | Safe Rust wrappers; explicit exposure and zeroization on drop. Does not provide locked memory or prevent every intermediate copy. | Secret strings/bytes and root-key ownership. Avoid enabling serialization of secret values. |
| **`zeroize`** | MIT OR Apache-2.0; official RustCrypto utilities project and CI linked in docs. [Documentation](https://docs.rs/zeroize/latest/zeroize/). | Portable volatile zeroization. Cannot erase earlier reallocations, every register/stack copy, swap, core dumps, or hardware side channels. | Temporary derived keys, plaintext buffers and explicit drop handling. Complements, not replaces, `secrecy`. |
| **SQLCipher + `rusqlite`** | `rusqlite`: MIT. SQLCipher Community: BSD-3-Clause-style terms; full notices must be redistributed. Commercial packages have different licenses. [rusqlite](https://github.com/rusqlite/rusqlite), [official community page linking source](https://www.zetetic.net/sqlcipher/community/), [exact license](https://www.zetetic.net/sqlcipher/license/SQLCIPHER_COMMUNITY_EDITION_LICENSE.txt). | Cross-platform but adds a native crypto/build surface. `bundled-sqlcipher` needs an appropriate crypto provider; vendored OpenSSL is another feature and license/dependency surface. SQLCipher replaces SQLite rather than acting as a normal loadable extension. | Whole-database privacy option, **not required for the secrets vault**. Do not switch Hutan’s entire SQLite stack just to encrypt API keys. |

### Backend details that matter

- **Windows:** the current native adapter defaults to **Enterprise** persistence. Set **Local** explicitly for Hutan’s local-only promise. Serialize operations on a given OS entry; upstream documents cross-thread sequencing issues. [Windows adapter docs](https://docs.rs/windows-native-keyring-store/latest/windows_native_keyring_store/).
- **macOS:** legacy Keychain and protected-data stores have different availability, signing and sandbox requirements. Use the legacy Keychain path initially to match Hutan’s existing signed-daemon integration; do not silently switch stores. [Apple adapter docs](https://docs.rs/apple-native-keyring-store/latest/apple_native_keyring_store/); existing signing requirement: `docs/adr/0008-credentials-and-auth-policy.md:19`.
- **Linux:** Secret Service generally needs a D-Bus session and an actual service implementation. Headless machines and WSL require special handling. KDE Wallet may require UTF-8 encoding for binary secrets; storing the wrapping key as base64 avoids that interoperability issue. [Secret Service adapter docs](https://docs.rs/zbus-secret-service-keyring-store/latest/zbus_secret_service_keyring_store/).
- `keyring-core`’s mock/sample stores are explicitly **not warranted secure or robust**. Never select either in production. [Core docs](https://docs.rs/keyring-core/latest/keyring_core/).

**License conclusion:** the named candidate components have compatible top-level license choices. Exact versions, enabled features, transitive dependencies, native-library licenses and distribution notices still need the normal dependency-policy check before adoption.

---

## 3. Vault options and D-6

| Option | Advantages | Disadvantages |
|---|---|---|
| Continue OS-store-only items | Smallest change; OS-native access controls | Host portability, backup, per-item policy and uniform audit remain awkward; Linux headless availability unresolved |
| **Encrypted item vault + OS wrapping** | Portable ciphertext, consistent policy/audit, local storage independent of backend, small OS item count | Same-user malware may access OS wrapping material; application crypto format needs review |
| **Vault + OS AND passphrase** | An unlocked OS account alone does not supply the missing passphrase | Interactive unlock; unattended jobs cannot use protected items after restart without a human |
| Vault + OS OR passphrase | Recovery and headless flexibility | Security is that of the weaker independent unlock path; not a second factor |
| Whole-database SQLCipher | Protects names, history, traces and other database content at rest | Locks ordinary Hutan operations to database unlock; native build/migration/performance work; does not solve authorization or runtime leakage |
| One age file as live vault | Reuses a standard portable format | Rewriting, locking, item concurrency, atomic updates and partial access become awkward |

**Recommendation for D-6:** ship OS wrapping as the convenient desktop default, with an optional **Require passphrase too** mode. Offer passphrase-only during explicit headless setup. An offline encrypted backup is recovery; do not automatically install a weaker online recovery wrapper.

All detailed designs below are proposals **[INFERENCE]**, grounded in the implementation and upstream contracts above.

---

## 4. Concrete cryptographic and storage design

### 4.1 Key hierarchy

Use three levels:

```text
OS wrapping key K_os ───────────────┐
                                   ├─ authenticated wrapper → vault root K_v
optional Argon2id key K_pass ───────┘

K_v → authenticated wrappers of random per-item keys K_item
K_item → authenticated encryption of item value + private metadata
```

- Generate every random key with the OS CSPRNG; 32-byte keys.
- Use XChaCha20-Poly1305 with a fresh random 24-byte nonce **for every encryption**, including rewrites and wrappers.
- Never derive a nonce from time, an item ID alone, or a counter that can roll back with a restored backup.
- Store algorithm identifiers and format versions; unknown algorithms fail closed.
- Construct AAD using a versioned, unambiguous binary encoding, not ad hoc string concatenation.

For the stronger mode:

```text
inner = AEAD(K_pass, K_v, AAD_pass)
outer = AEAD(K_os, inner, AAD_os)
```

Both layers are required. Do not leave an OS-only root wrapper installed when enabling this mode.

For headless passphrase-only mode, persist only the passphrase wrapper. Absence of the OS store is an explicit setup choice, not an automatic downgrade.

### 4.2 Argon2id

Initial proposed profile:

- Argon2id version `0x13`.
- Random 16-byte salt.
- 32-byte output.
- 64 MiB memory, three passes, four lanes.
- Store the exact parameters with the wrapper.
- Run one bounded derivation at a time per vault on a blocking worker.
- Reject unreasonable imported parameters before allocating; expose an explicit override for a legitimate stronger backup rather than accepting attacker-controlled resource costs.

The 64 MiB / three-pass / four-lane profile follows RFC 9106’s second recommended option. Actual unlock latency on supported hardware remains to be measured; do not claim it is already acceptable. [RFC 9106 §4, §7.4](https://www.rfc-editor.org/rfc/rfc9106).

### 4.3 Proposed SQLite schema

Use a separate `vault.sqlite` in the daemon data directory, restrictive permissions/ACL, with SQLite receiving **ciphertext only** for secret-bearing fields.

| Table | Important columns |
|---|---|
| `vault_meta` | `vault_id`, `format_version`, `active_root_generation`, `unlock_mode`, `created_at_ms`, `migration_state` |
| `vault_root_wrappers` | `generation`, `mode`, `os_backend`, `os_account_ref`, `kdf_algorithm`, `kdf_version`, `kdf_params`, `salt`, `nonce`, `wrapped_root`, `created_at_ms` |
| `vault_items` | `item_id`, `revision`, `root_generation`, `name_lookup`, `key_nonce`, `wrapped_item_key`, `value_nonce`, `sealed_value`, `created_at_ms`, `updated_at_ms` |
| `vault_grants` | `grant_id`, `item_id`, `revision_constraint`, `workspace_id?`, `connector_id?`, `use_kind`, `destination_policy`, `approval_policy`, `expires_at_ms?` |
| `vault_audit` | `audit_id`, `operation_id`, `ts_ms`, `item_id?`, `revision?`, `actor_kind`, `actor_id?`, `session_id?`, `turn_id?`, `use_kind`, `destination_id?`, `decision`, `outcome`, `trace_id?` |
| `vault_migrations` | `source_kind`, `source_id`, `item_id`, `phase`, `updated_at_ms`, sanitized `error_code?` |

Private item payload:

```json
{
  "name": "provider/openai/api-key",
  "kind": "api_key",
  "value": "<secret bytes>",
  "notes": "<optional private note>"
}
```

`name_lookup` is an HMAC of the canonical name under a domain-separated lookup key derived from `K_v`, using standard HKDF/HMAC implementations. Names and notes therefore need not be exposed in raw database copies. Listing names requires unlock.

AAD binds:

- Vault ID and format version.
- Item ID.
- Item revision.
- Payload/key-wrapper purpose.
- Root generation for root-key wrappers.

A row swap between items or substitution of a payload from another vault must fail authentication.

**Honest metadata leakage:** row counts, ciphertext lengths, timestamps, opaque item IDs and some grant/audit relationships remain visible. Padding can reduce length leakage, but should not be presented as complete traffic-analysis protection.

The encrypted item’s name must be checked against the requested canonical name after lookup; the HMAC index is an accelerator, not an authorization decision.

### 4.4 What rotation means

Expose distinct operations:

1. **Change passphrase:** rewrap the root; no item-value rewrite.
2. **Replace OS wrapping key:** create a new OS entry, persist and verify the replacement wrapper, then remove the old entry.
3. **Rotate root key:** rewrap item keys and rebuild keyed name indexes transactionally/resumably; item ciphertext can remain unchanged.

[Showing lines 1-200 of 766. Use :201 to continue]
4. **Rotate item encryption key:** decrypt/re-encrypt that item under a new random item key.
5. **Rotate provider credential:** replace the credential value. This is different from local encryption-key rotation.

A leaked old root plus old database backup remains sufficient to decrypt that backup. Rewrapping today’s database does not retroactively revoke stolen copies. Suspected credential compromise requires rotation at the provider.

---

## 5. References, authorization and use-time resolution

### Reference contract

Define `secret://name` as a Hutan reference, not an arbitrary network URL:

```text
secret://provider/openai/api-key
secret://connector/github/token
```

- Canonical restricted name syntax; reject query, fragment, percent-encoding ambiguity and traversal-like names.
- Resolve only exact reference fields or explicitly declared secret-binding fields.
- No recursive interpolation in ordinary strings, shell commands, prompt text or files.
- Persist the reference, never the resolved value.
- Names are immutable identifiers initially; changing a display label does not rename the reference.
- References belong to the selected **daemon host**. No automatic local-to-remote forwarding.

### Allowed sinks

| Sink | Resolution point and policy |
|---|---|
| Child environment | Immediately before spawn. Persist a separate `secret_env` binding map; resolve after authorization. Never place secrets in argv or a generated shell command. |
| Provider header | Immediately before constructing/sending the authenticated request. Bind grant to provider configuration and approved destination origin. Disable credential forwarding across redirects; changing the destination invalidates approval. |
| MCP tool arguments | Immediately before the daemon sends the request to a registered connector. Bind item use to connector identity, tool name, argument JSON pointer and configured destination. |

**Critical MCP boundary:** Hutan cannot substitute references inside arbitrary external MCP traffic owned by an ACP harness unless it actually proxies that traffic. The design therefore requires a daemon-owned connector/egress path for secret-bound calls. A harness’s unrelated native MCP connection is not automatically covered.

### No secret-reading tool

Agents may receive permitted item references and non-sensitive descriptions, but never a value-returning `vault.get` / `secret.read`.

A use request must be evaluated against:

- Authenticated origin and existing `[control]` policy.
- Workspace/session scope.
- Requested sink.
- Exact destination/connector identity.
- Item ID and revision.
- Approval lifetime.

The daemon resolves a name to a stable item/revision **before approval**, then revalidates it before use. Replacing an item or changing a connector endpoint must not reuse approval for a different destination or credential.

For arbitrary MCP parameters, authorization is necessarily a trust decision about the receiving connector: once delivered, that server has the plaintext.

### Audit behavior

Record allowed and denied uses without values. Persist an authorized-use intent before releasing plaintext to a sink; if the security audit cannot be written, fail the secret use closed. Record completion separately.

A crash between intent and completion yields **outcome unknown**, not “success.” Local audit is useful but not tamper-proof against the account owner, root, or a compromised daemon.

---

## 6. Redaction: strengthen the boundary, preserve honest limits

Move the shared non-I/O redaction machinery out of the index into a small `hutan-privacy` module/crate, then migrate existing callers. The index must not become a dependency required just to sanitize secrets.

Use three layers:

1. **Structural non-collection**
   - Secret types cannot serialize or print.
   - No request/response body logging at secret egress.
   - Trace attributes use a typed allowlist.
   - Raw provider payloads are not copied into traces.
   - Secret arguments become reference/redaction markers in the journal before dispatch.

2. **Known-value redaction**
   - Operation-local scrubber knows values released to that operation.
   - Scrub errors and outputs before persistence or publication.
   - Keep scrubbers alive for the lifetime of credential-bearing children, including after vault lock.
   - Cover partial snapshots, approval data, provider payloads, final events, notifications, crash/error reporting and index ingestion—not just final message text.

3. **Existing heuristic redaction**
   - Preserve recognizable token/header/assignment patterns for secrets Hutan did not supply.
   - Apply before truncation where feasible.
   - Stream processing must handle values split across chunks. A bounded overlap buffer or complete-item sanitization is required; independent regex passes over arbitrary chunks are not enough.

The current journal handle path folds events into partials before the final journal/publish phase (`crates/daemon/src/chat.rs:1587-1635,1746-1754`). The implementation needs an explicit sanitized event projection at this boundary, plus safe handling of streaming tails.

**Limits:**

- Exact replacement does not stop base64, hashing, encoding, partial leakage or semantic transformations by a malicious child/tool.
- Terminal output, files written by a child, native harness transcripts and external connector logs may live outside Hutan’s control.
- Short passwords are particularly unsuitable for global substring masking.
- A powerful agent that can execute arbitrary same-user code can often bypass application policy.

Therefore the security statement is **reduced exposure and controlled daemon-mediated use**, not “prompt injection cannot steal secrets.”

---

## 7. Unlock, lock, backup and threat model

### UX

Vault states:

```text
uninitialized | locked | unlocking | unlocked | unavailable | recovery_required
```

- Setup explains chosen protection and recovery consequences.
- OS-only mode may unlock on the first explicit credential-bearing operation.
- Passphrase modes use a secure text field or non-echoing CLI input; never command-line flags, shell history or environment variables for the passphrase.
- Agent-origin requests cannot unlock, change protection, export, create broad grants or retrieve values.
- Show “Needs vault unlock” as actionable state. Do not silently stall schedules.
- Lock command immediately denies new resolutions, cancels pending unlock/use approvals and drops cached root/derived keys.
- In-flight network requests may already possess credentials; running children retain injected environments. Show this limitation in the lock dialog and offer a separate “Stop credential-bearing sessions” action.
- Lock on daemon shutdown. Desktop auto-lock can additionally respond to OS screen-lock events; remote hosts cannot reliably infer the local desktop’s screen state.

### Proposed settings

```toml
[vault]
unlock_on_use = true
idle_lock_secs = 900
lock_on_screen_lock = true
allow_remote_management = false
max_item_bytes = 65536

[traces]
enabled = true
retention_days = 30
max_bytes = 268435456
hard_max_bytes = 335544320
capture_content = false

[traces.otlp]
enabled = false
endpoint = ""
protocol = "http/protobuf"
headers = {} # values may be secret:// references
export_identifiers = false
```

Protection mode and KDF parameters belong to authenticated vault metadata and explicit management commands, not an editable setting that can accidentally downgrade encryption.

Idle-lock time is measured since a successful authorized use. Users choosing always-active automation must understand that frequent use can keep an OS-only vault unlocked. Passphrase-only unattended restart remains intentionally unsupported without human unlock.

### Backup/export

Offer two clearly different products:

1. **Encrypted snapshot backup**
   - Consistent SQLite snapshot, including encryption metadata.
   - Recovery depends on retained wrapping credentials unless exported with independent recovery protection.
   - Copying only the main database file during WAL activity is not a valid backup procedure.

2. **Portable age export**
   - Authorized desktop/CLI action.
   - Stream a versioned logical bundle directly into the `age` library’s encrypted writer; no plaintext temporary archive.
   - Prefer explicit X25519 recipient encryption.
   - If password-based age export is offered, state that age uses scrypt; do not label it Argon2id.
   - No automatic plugin execution.
   - Restore validates sizes, format, duplicates and authentication before committing; conflicts require explicit policy.
   - Create new local root/item keys and OS wrappers on import rather than cloning the source host’s identity and permissions.
   - Grants are disabled on another host until reviewed.

Backup deletion and vault deletion do not guarantee erasure from SSD history, filesystem snapshots or old backups.

### Threat model

| Threat | Protection / limit |
|---|---|
| Copied vault database or backup | AEAD protects item values; independent backup protection is needed for portability |
| Other local OS users | Permissions/ACL plus encryption and OS store isolation |
| Stolen powered-off disk | Depends also on full-disk encryption, OS key-store protection and passphrase strength |
| Offline password guessing | Argon2id raises cost; weak passphrases remain weak |
| Accidental logs/index/export | Non-collection, typed secret wrappers and layered redaction reduce exposure |
| Prompt injection through daemon tools | Scoped references and destination-bound grants reduce capability; not a full sandbox |
| Same-user malware | May read OS-store material, impersonate clients, inspect child environments or access unlocked daemon memory |
| Root/admin, compromised kernel/daemon | Out of protection scope |
| Host rollback | AEAD detects corruption, not restoration of an old valid vault; preventing rollback requires an independent trusted monotonic state |
| Remote server compromise | Credentials on that server are exposed; SSH encryption does not make the server trustworthy |

`secrecy` and `zeroize` improve hygiene, not a guarantee against memory scraping or core dumps. Their own documentation makes these limits explicit.

---

## 8. Traces: standards and span model

### Upstream status

The official OpenTelemetry site now points GenAI conventions to the dedicated **[open-telemetry/semantic-conventions-genai](https://github.com/open-telemetry/semantic-conventions-genai)** repository. These conventions are marked **Development**, not stable:

- [Client inference](https://github.com/open-telemetry/semantic-conventions-genai/blob/main/docs/gen-ai/client-inference.md).
- [Agent/framework spans](https://github.com/open-telemetry/semantic-conventions-genai/blob/main/docs/gen-ai/gen-ai-agent-spans.md).
- [Tool spans and content handling](https://github.com/open-telemetry/semantic-conventions-genai/blob/main/docs/gen-ai/gen-ai-spans.md).
- [MCP](https://github.com/open-telemetry/semantic-conventions-genai/blob/main/docs/gen-ai/mcp.md).

Pin an exact convention revision in Hutan’s ADR and instrumentation metadata. Do not make mutable upstream names the database schema.

Use standard 16-byte trace IDs and 8-byte span IDs, encoded as lowercase hex on JSON wire. [OTel Trace API](https://opentelemetry.io/docs/specs/otel/trace/api/#spancontext).


[Showing lines 201-400 of 766. Use :401 to continue]
### Proposed span tree

```text
invoke_agent <backend>                 one user/harness turn
├─ chat <model>                        only when actual inference is visible
├─ execute_tool <tool>
│  ├─ hutan.approval.wait
│  └─ tools/call <tool>                merge with tool span where same operation
├─ hutan.delegate                      dispatch / wait, not invented inference
│  └─ child invoke_agent              parent or linked trace
└─ hutan.secret.use                    metadata only

hutan.schedule.run                     one durable occurrence
└─ invoke_agent <backend>
```

| Operation | Attributes / behavior |
|---|---|
| Turn | `gen_ai.operation.name=invoke_agent`; backend identity, actual model if known, conversation ID, Hutan session/workspace/turn IDs; end on completion, cancel, failure or interrupted daemon |
| Direct inference | `gen_ai.operation.name=chat` or the actual supported operation; provider/request/response model, response ID, input/output/cache/reasoning tokens and first-chunk latency when known |
| Tool | `gen_ai.operation.name=execute_tool`, `gen_ai.tool.name`, call ID, tool type and safe outcome; no raw command or arguments by default |
| Approval | Hutan-specific span measuring wait separately from active execution; approval ID, policy source, outcome, deciding client class |
| MCP | `mcp.method.name`, tool name, protocol version, safe server identity, JSON-RPC error code; `isError=true` maps to `error.type=tool_error` |
| Delegation | Stable parent/child session IDs and dispatch outcome; use links for detached work, cross-trace continuation or fan-in |
| Schedule | Occurrence/job IDs, scheduled versus actual start, lateness, selected policy, skipped/blocked/completed outcome |
| Vault | Item ID/revision, sink category, grant/approval decision and outcome; no value, header, passphrase, or secret name required |

OTel MCP guidance explicitly recommends adding MCP attributes to an already-existing tool-execution span instead of creating a duplicate when the operation is reliably the same. Follow that guidance.

MCP propagation uses `params._meta.traceparent` / `tracestate`, not only an HTTP header. Do not propagate baggage by default. Treat externally supplied trace context as correlation data, never as authorization.

### External harness visibility

Hutan can time the enclosing turn and exposed tool/approval lifecycle. It cannot truthfully produce spans for hidden model requests, native tools or subagents that the adapter does not report.

Mark coverage:

```text
observed | adapter_reported | reconstructed
```

An ACP `ToolStart` is evidence of a reported tool, not proof that Hutan executed or sandboxed it. Missing internals remain missing. Never manufacture inference spans from text deltas or assume one turn equals one provider request.

### Local timing and restart

- Record epoch timestamp plus monotonic elapsed duration.
- Do not compute latency solely by subtracting wall-clock timestamps.
- Persist span starts as well as ends so live viewing and restart recovery work.
- On restart, mark unfinished spans `interrupted` with a Hutan reason; do not invent successful completion or precise post-crash duration.
- Replay must be idempotent. Journal sequence is provenance, not a globally unique OTel span ID.

---

## 9. Trace storage, bounds, accounting and export

### SQLite tables

Use separate `traces.sqlite` so retention/checkpoint work does not contend directly with chat journaling.

| Table | Main fields |
|---|---|
| `traces` | `trace_id BLOB(16)`, root span, session/turn, workspace-at-start, start/end, outcome, source, instrumentation/convention revision |
| `spans` | `(trace_id,span_id)` PK, parent span, operation, kind, start/end, duration, status, sanitized error code, bounded attributes, source provenance |
| `span_events` | span key, event ordinal/time, allowlisted name and attributes |
| `span_links` | span key, linked trace/span IDs, relation |
| `trace_sources` | span key, journal session/sequence range or daemon operation ID; unique ingestion key |
| `trace_retention_state` | pruning cursor, dropped count, last storage error, bytes accounted |
| `usage_records` | separate durable accounting ledger described below |

Indexes: session/start, workspace-at-start/start, model/start, outcome/start, parent span, and trace-start ordering. Avoid indexing arbitrary JSON attributes.

Workspace attribution is captured at operation start. Moving a session later must not silently rewrite historical cost ownership; a separate UI filter can follow current membership.

### Proposed initial bounds

- Thirty-day detailed retention.
- 256 MiB soft target; 320 MiB hard storage ceiling.
- Bounded transaction batches and a bounded ingress queue.
- At most 64 attributes per span, 16 KiB aggregate attribute payload, 32 events and 16 links initially.
- Pagination ≤200 rows; long trace details fetched by cursor.
- No per-token or per-delta span writes.
- Attribute truncation is flagged, not hidden.

Physical limits include SQLite, WAL and maintenance headroom—not merely `SUM(length(attributes))`. `journal_size_limit` alone is not a hard bound during active readers.

Implement page-count admission limits, bounded WAL transactions, short-lived read transactions, checkpointing and pruning before admission. If readers/storage prevent safe progress, stop admitting detailed telemetry and expose `traces.degraded`; do not allow unbounded WAL growth or block interactive turns.

Retain completeness/drop markers so a partial trace is not presented as complete. Vault audit is a separate security record and must not be silently dropped with traces.

### Token and cost accounting

Use an idempotent usage ledger in the main storage service, independently retained from detailed traces:

```text
usage_records(
  source_key PRIMARY KEY,
  session_id, turn_id, workspace_id_at_use,
  backend_id, provider_id?, model?,
  request_id?, observed_at_ms,
  input_tokens?, cache_read_tokens?, cache_write_tokens?,
  output_tokens?, reasoning_tokens?,
  source_semantics, completeness,
  price_revision?, currency?,
  estimated_cost_nanos?, billed_cost_nanos?
)
```

- A provider request contributes once, not once on the inference span and again on the enclosing turn.
- Cached input is a subset of total input; reasoning output is a subset of total output. Do not add subsets again.
- Normalize provider-specific meanings at the adapter boundary.
- Do not sum successive cumulative conversation snapshots. Prefer request-scoped usage; cumulative differencing requires a known baseline and reset handling.
- When a harness exposes only aggregate turn usage, label the record aggregate and do not also add observed child usage into the same total.
- Unknown counts/cost remain unknown, following existing `TokenCounts` semantics.
- Costs use versioned prices and integer fixed-point arithmetic. Preserve price source/effective date and distinguish estimate from billed amount.
- Subscription-backed harness tokens do not imply a known marginal dollar cost.
- Local models can have known token counts and unknown cost.
- Pruning detailed traces must not change previously reported totals. Give accounting its own documented retention, proposed one year, and an explicit clear/export control.

### OTLP choice

Use official `opentelemetry`, `opentelemetry_sdk` and `opentelemetry-otlp`, Apache-2.0. The official exporter supports gRPC and HTTP/protobuf; its trace exporter is currently described as beta. [Official Rust repo](https://github.com/open-telemetry/opentelemetry-rust), [exporter docs](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/).

Prefer **HTTP/protobuf** initially to avoid adding tonic solely for optional telemetry. Disable unused signals/default features and use the existing rustls-oriented HTTP stack where compatible.

Export only after explicit enablement:

- TLS required except explicit loopback development endpoints.
- Endpoint/header configuration must not be silently enabled by inherited `OTEL_*` environment variables.
- Secret header values resolve through the vault.
- Short timeout, bounded batches, bounded queue; no indefinite retry spool.
- Export recent completed spans only by default; historical backfill is a separate user action.
- Redact before local persistence and again apply an export allowlist.
- Omit transcript content, paths, secret names and raw internal identifiers by default.
- Local identifiers may be mapped to installation-scoped pseudonyms for exported correlation.
- No collector required for local operation.

Export discloses activity timing, tool names and model usage even without content. The settings UI must explain that boundary.

---

## 10. Trace viewer

Provide the same viewer from session, workspace and Assistant Home:

1. **List:** time, backend/model, outcome, duration, reported tokens, estimated/billed cost and completeness.
2. **Waterfall:** turn, inference, tools, approval wait, delegation and schedule relationships.
3. **Details:** safe attributes, provenance, error classification and links to journal locations.
4. **Usage:** workspace/backend/model/date totals, unknown-coverage indicators, pricing revision.
5. **Controls:** pause recording, retention, clear local traces, explicit sanitized export and OTLP settings.

Implementation constraints:

- Virtualized rows and lazy detail fetch.
- No periodic refresh when nothing changes.
- Coalesced invalidation, not one UI event per token/span mutation.
- Keyboard navigation and textual alternatives to color.
- Distinguish waiting for approval from active execution.
- Show “not observed,” “reported by harness,” “interrupted,” and “recording dropped” honestly.
- Journal links must work when the referenced session exists and degrade cleanly when it has been deleted.

---

## 11. Protocol and module layout

Current source declares protocol **0.29** (`crates/protocol/src/lib.rs:68-72`). Direction already proposes other additions; allocate the next free minor during implementation rather than reserving a conflicting number here.

ADR 0002 permits additive, capability-gated evolution and requires existing frame size, sequencing, replay, snapshot and backpressure behavior (`docs/adr/0002-ipc-transport-and-framing.md:20-39`).

### Additive capabilities

```text
vault.status
vault.manage
vault.bindings
traces.query
traces.usage
```

Management capability advertisement is not authorization. Origin policy must separately deny agent access to secret-bearing management commands.

### Proposed requests

```text
vault.status {}
vault.initialize {mode, passphrase?}
vault.unlock {passphrase?}
vault.lock {}
vault.items.list {cursor?, limit}
vault.item.put {name, kind, value, expected_revision?}
vault.item.delete {item_id, expected_revision}
vault.grants.list {item_id?}
vault.grant.put {...destination-bound policy...}
vault.grant.revoke {grant_id}
vault.rotate {kind, ...}
vault.backup {destination, recipients}
vault.restore {source, conflict_policy}

trace.list {scope, from_ms?, to_ms?, outcome?, cursor?, limit}
trace.get {trace_id, cursor?, limit}
trace.usage {scope, group_by, from_ms, to_ms}

[Showing lines 401-600 of 766. Use :601 to continue]
trace.status {}
trace.clear {scope, before_ms?}
```

Secret-bearing request parameters need dedicated non-logging types. Never include their raw JSON in generic tracing, errors or request audit.

Examples:

```json
{
  "id": 81,
  "method": "trace.list",
  "params": {
    "scope": {"session_id": "…"},
    "limit": 100
  }
}
```

```json
{
  "id": 81,
  "result": {
    "seq": 4201,
    "traces": [],
    "next_cursor": null,
    "coverage": "observed"
  }
}
```

Events contain metadata/invalidation only:

```text
vault.state_changed {state, generation}
vault.items_changed {revision}
trace.changed {scope, revision}
trace.storage_degraded {reason_code, dropped_count}
```

Publish only to clients that negotiated the relevant capability. Responses stay below 4 MiB; backup content is not stuffed into a control frame.

Errors include `vault_locked`, `vault_unavailable`, `secret_not_found`, `secret_use_denied`, `revision_conflict`, `unlock_failed`, and `trace_storage_full`, with no secret-bearing detail.

### Crate/module ownership

```text
crates/platform/src/secrets/
    OS wrapping-key backends only

crates/vault/src/
    format.rs       key hierarchy, AEAD/AAD
    store.rs        encrypted SQLite rows
    unlock.rs       state machine and bounded KDF
    policy.rs       grants and destination binding
    audit.rs
    backup.rs

crates/privacy/src/
    secret.rs       non-printing/zeroizing values
    redact.rs       shared structured/heuristic sanitization

crates/daemon/src/
    vault.rs        authoritative service, approvals, origin policy
    traces.rs       instrumentation coordination
    providers.rs   use-time vault binding
    connector/...  daemon-owned secret-bound MCP egress

crates/storage/
    usage ledger and provider-reference migration

crates/observability/src/
    model.rs
    store.rs
    accounting.rs
    otlp.rs

crates/protocol/src/
    vault.rs
    trace.rs

crates/desktop/src/...
    vault management/unlock UI and trace viewer
```

Do not import the OS store into protocol, desktop or core. Do not let the vault depend on GPUI or an OTLP collector.

---

## 12. Migration and compatibility

### Provider keys

ADR 0008 currently mandates Keychain-only storage. Replace that storage decision with the vault ADR while retaining its rules against extracting vendor subscription credentials (`docs/adr/0008-credentials-and-auth-policy.md:11-15`).

Migration:

1. Initialize vault and its wrapping material explicitly.
2. Enumerate **configured provider IDs**, not all Keychain contents.
3. For each provider, read only its existing `dev.hutan.provider` entry.
4. Insert encrypted vault item and verify decryption in memory.
5. Commit provider `credential_ref=secret://provider/{id}/api-key` plus migration state.
6. Only after durable commit, delete the legacy provider Keychain entry.
7. Record failed cleanup and retry idempotently.

A crash before pointer commit leaves the old source authoritative; a crash after commit leaves the vault authoritative and the old item as pending cleanup. Never delete the only usable copy first.

OS prompts/refusal pause migration with a useful state. Do not synthesize “missing credential” and erase the reference on a transient store error.

After successful migration, normal operation must not dual-read old and new stores. The migration reader remains only for unmigrated records and is removed after the supported migration window.

### Wire compatibility

These are **protocol-minor** changes:

- New capability-gated methods/events.
- Optional trace references or secret-binding maps.
- Internal database pointer migration.
- Keeping `provider.set_credential` as the provider-specific credential-setting operation, now writing the vault, without changing its existing meaning.

Do not reinterpret arbitrary existing environment strings as secret references. Add an explicit binding field so old clients’ literal strings retain their meaning.

A **major** is required if implementation instead changes required existing message shapes, removes methods that older supported clients need, or changes established semantics incompatibly. New enum variants must be gated where older deserializers cannot tolerate them.

No framing, replay, terminal-credit or queue behavior changes are needed.

### Trace migration

- New traces begin from activation.
- Optional historical reconstruction can use journaled start/end/tool/approval events, with deterministic ingestion keys.
- Mark reconstructed spans and missing coverage.
- Existing wall-clock timestamps do not justify invented monotonic latency, hidden inference requests or historical pricing.
- Do not export reconstructed history automatically.

---

## 13. Implementation slices and acceptance tests

The following are acceptance criteria for future implementation, **not checks exercised by this research**.

| Slice | Deliverable | Acceptance |
|---|---|---|
| **V-1 — ADR, format and crypto** | Vault ADR; key hierarchy; versioned AAD/schema; secret types | Known-vector tests; wrong key/passphrase fails; nonce/ciphertext/AAD tampering and cross-item/vault substitution fail; unknown version fails closed; no secret `Debug`/serialization |
| **V-2 — OS backends and unlock** | Explicit keyring adapters; OS/passphrase/AND state machine; headless mode | Isolated test stores on all three OSes; unavailable store never falls back; Windows Local persistence; serialized entry operations; wrong passphrase bounded; lock denies new uses; restart begins locked |
| **V-3 — Provider migration** | Durable migration ledger and provider reference cutover | Fault injection after every phase; no key loss; refusal is resumable; cleanup retry idempotent; no normal dual-read; mock stores only in tests |
| **V-4 — Bindings and policy** | Environment/header/daemon-owned MCP resolution | Database and config contain refs only; destination mutation invalidates grant; item-revision TOCTOU denied; redirect cannot forward credential; agent cannot read/unlock/export; remote refs resolve only on selected host |
| **V-5 — Privacy boundary** | Shared sanitizer; journal/partials/index/errors | Arbitrary-shaped canary absent from database, WAL, logs, index, replay and exports; split-chunk canary covered; approval/error payload coverage; no secret request debug; output-transform limitations documented |
| **V-6 — Management and recovery** | Unlock/lock UI, rotation, age export/restore | All modes explained; running-child limitation shown; backup decrypts independently; no plaintext temp archive; truncated/tampered/oversized import rejected atomically; rotated-root and old-backup semantics demonstrated |
| **T-1 — Span model and ingestion** | Trace IDs, provenance, turn/tool/approval/MCP spans | Idempotent replay; correct overlapping tools; approval wait separate; cancel/failure/interruption correct; no fake inference spans; MCP tool span deduplication |
| **T-2 — Bounded storage** | SQLite store, retention, paging, degraded state | High-volume/fault tests; bounded ingress and WAL/disk use; slow readers cannot grow storage indefinitely; retention deletes complete traces; interactive turns continue when detailed tracing stops |
| **T-3 — Accounting** | Idempotent usage ledger and versioned prices | Cumulative snapshots not summed; cache/reasoning subsets not double-counted; parent/child aggregates not duplicated; unknown remains unknown; pruning traces preserves totals; workspace moves preserve historical attribution |
| **T-4 — Viewer** | Scoped list/waterfall/details/usage pages | Virtualized large datasets; keyboard accessibility; no idle polling; honest missing coverage; deleted journal links degrade cleanly; bounded snapshot/reconnect behavior |
| **T-5 — Optional OTLP** | Official HTTP/protobuf exporter | Disabled means no network traffic despite inherited OTEL env; TLS and destination handling; vault headers never exported; collector outage stays bounded; explicit opt-in for historical export; local trace viewing unaffected |

### Highest risks to resolve before release

1. **Security claims exceeding the actual boundary:** a same-user native harness is not a sandbox.
2. **Passphrase downgrade mistakes:** independent wrappers accidentally bypass the stronger mode.
3. **Plaintext through secondary paths:** partial journals, provider payloads, subprocess errors, native transcripts and crash reporting.
4. **Credential destination confusion:** user-approved references later sent to changed provider/MCP endpoints.
5. **Migration data loss:** deleting Keychain entries before the vault and reference are durably committed.
6. **False accounting precision:** unknown harness usage, cumulative counters and changing model prices.
7. **Telemetry-driven lag:** per-delta writes, unbounded exporter queues and long-lived SQLite readers.
8. **Upstream evolution:** current keyring architecture and GenAI conventions have both changed; pin versions/revisions and isolate adapters.

**Bottom line:** build a small, auditable cryptographic vault and a bounded metadata-first trace system around the daemon’s existing authority. The defensible promise is “encrypted local credentials, controlled use, reduced accidental disclosure, and visible execution,” not “nobody can steal secrets” or “every backend’s internals are observable.”