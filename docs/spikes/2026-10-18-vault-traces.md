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
| **`age` / `rage`** | Rust implementation: MIT OR Apache-2.0. `age` 0.12.1 is still described as beta; changelog records July 2026 fixes and a previously fixed plugin-execution vulnerability. Official age project links to `str4d/rage` as its interoperable Rust implementation. [age site](https://age-encryption.org/), [Rust repo](https://github.com/str4d/rage), [changelog](https://raw.githubusercontent.com/str4d/rage/main/age/CHANGELOG.md). | Rust library and binaries for macOS, Linux and Windows; file-oriented streaming encryption. Native password encryption uses **scrypt**, not Argon2id. Plugins can execute programs. | Portable encrypted export/backup, with plugins and SSH features disabled unless a separately approved requirement exists. Not the live item …
| **`chacha20poly1305`** | MIT OR Apache-2.0. Official RustCrypto repository contains algorithm-specific CI; docs report an NCC audit with no significant findings. That audit is historical, not an audit of Hutan or every current dependency. [Repository](https://github.com/RustCrypto/AEADs), [documentation](https://docs.rs/chacha20poly1305/latest/chacha20poly1305/). | Pure Rust; XChaCha uses a 24-byte nonce. Documented constant-time caveat on processors with variable-time multiplication. Appropriate target assumptions must be explicit. | **Preferred item and wrapping AEAD**, using full-round XChaCha20-Poly1305. |
| **`aes-gcm`** | MIT OR Apache-2.0; same RustCrypto maintenance/audit qualifications. [Documentation](https://docs.rs/aes-gcm/latest/aes_gcm/). | Pure Rust with hardware acceleration where available; default nonce is 12 bytes. Unique-nonce management needs particular care. | Valid alternative when interoperability or a defined compliance requirement demands AES-GCM. Merely using this crate does **not** establish FIPS certification. |
| **`argon2`** | MIT OR Apache-2.0. Official changelog records 0.6.0 on 2026-08-27, including allocation-failure handling and dependency/API updates. [Repository](https://github.com/RustCrypto/password-hashes/tree/master/argon2), [changelog](https://raw.githubusercontent.com/RustCrypto/password-hashes/master/argon2/CHANGELOG.md). | Pure Rust, Argon2id default; supports no-std configurations. Derivation consumes intentional memory/CPU and belongs off the async reactor. | Passphrase-to-wrapping-key derivation using `hash_password_into`, not a serialized password-verifier string. |
| **`secrecy`** | MIT OR Apache-2.0. Current documented 0.10.3; latest inspected changelog release is 2024-10-09. Low release frequency is observable; it is not by itself proof of abandonment. [Repository](https://github.com/iqlusioninc/crates/tree/main/secrecy), [changelog](https://raw.githubusercontent.com/iqlusioninc/crates/main/secrecy/CHANGELOG.md). | Safe Rust wrappers; explicit exposure and zeroization on drop. Does not provide locked memory or prevent every intermediate copy. | Secret strings/bytes and root-key ownership. Avoid enabling serialization of secret values. |
| **`zeroize`** | MIT OR Apache-2.0; official RustCrypto utilities project and CI linked in docs. [Documentation](https://docs.rs/zeroize/latest/zeroize/). | Portable volatile zeroization. Cannot erase earlier reallocations, every register/stack copy, swap, core dumps, or hardware side channels. | Temporary derived keys, plaintext buffers and explicit drop handling. Complements, not replaces, `secrecy`. |
| **SQLCipher + `rusqlite`** | `rusqlite`: MIT. SQLCipher Community: BSD-3-Clause-style terms; full notices must be redistributed. Commercial packages have different licenses. [rusqlite](https://github.com/rusqlite/rusqlite), [official community page linking source](https://www.zetetic.net/sqlcipher/community/), [exact license](https://www.zetetic.net/sqlcipher/license/SQLCIPHER_COMMUNITY_EDITION_LICENSE.txt). | Cross-platform but adds a native crypto/build surface. `bundled-sqlcipher` needs an appropriate crypto provider; vendored OpenSSL is another feature and license/dependency surface. SQLCipher replaces SQLite rather than acting as a normal loadable extension. | Whole-database privacy option, **not required for the secrets vault**. Do not switch Hutan’s…

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
…

…

…
**Bottom line:** build a small, auditable cryptographic vault and a bounded metadata-first trace system around the daemon’s existing authority. The defensible promise is “encrypted local credentials, controlled use, reduced accidental disclosure, and visible execution,” not “nobody can steal secrets” or “every backend’s internals are observable.”

[Backing file: ~/.omp/agent/sessions/-proj-heeler/2026-10-15T13-22-59-393Z_01a13fba-f4c1-715f-8096-a302709d439a/ResearchVaultTraces.md (50.2KB). Use agent://ResearchVaultTraces:raw:1-3000 for bounded verbatim chunks, agent://ResearchVaultTraces:1-3000 for numbered exploration, and the backing file path for search/copy workflows.]

[Showing lines 1-300 of 766. Use :301 to continue. Some lines truncated to 768 chars. Use agent://ResearchVaultTraces:raw:72-72 to read line 72 whole; cut lines run to line 78, each read the same way]