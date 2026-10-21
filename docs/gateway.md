# Direct model chat (the model gateway)

Decisions: ADR 0008 (credentials, normative), ADR 0005 (`AgentEvent`), ADR 0006 (SQLite). Wire messages: `docs/protocol.md` § Direct model chat and providers. Roadmap: M5.1–M5.3 (the desktop chat view for it is M5.4, reusing the M4 chat view). Agent chats (Codex) are `docs/agents.md`.

A direct chat is a conversation with a model of a provider — OpenAI, Anthropic, Ollama, or anything that speaks the OpenAI chat API — with no agent in between: no tools, no files, no approvals. The daemon makes the HTTP request, streams the reply, and journals it as the same `AgentEvent`s a Codex session produces, so the CLI today and the chat view later show both alike. The API key lives in the macOS Keychain, written and read only by the daemon.

## Parts

| Where | What |
|---|---|
| `crates/gateway` | `Provider` trait (`list_models`, `stream_chat`), the three adapters, `Http` (timeouts, connect retry, status mapping), `CredentialStore` (`KeychainStore`, `MemoryStore`), `Secret`, local discovery, a scriptable mock HTTP server for tests (feature `test-support`) |
| `crates/daemon/src/providers.rs` | `ProviderService`: provider CRUD, credentials, `provider.models`, `provider.discover`; resolves a provider to a ready adapter |
| `crates/daemon/src/direct.rs` | `DirectSession`: one per running direct chat; builds the request from the journal, streams a turn, maps it to events, interrupts, switches model |
| `crates/daemon/src/chat.rs`, `registry.rs` | the same pump, journal and session lifecycle as Codex chats (`ChatManager` holds a `Backend::Direct`); `chat.create` / `chat.set_model` |
| `crates/storage` (schema v5) | `providers` table (no secret) |
| `crates/client`, `crates/cli` | `Client::provider_*`, `chat_set_model`; `hutan provider …`, `hutan chat new --direct`, `hutan chat model` |

## Providers

A provider is `{id, kind, name, base_url, default_model, keychain_ref, created_at}` in SQLite. The `id` is a slug the daemon makes from the name (`OpenRouter` → `openrouter`, `-2` appended when taken); it is what every command and the Keychain account call the provider.

| kind | Speaks | Default base URL | Needs a key |
|---|---|---|---|
| `openai` | OpenAI Chat Completions | `https://api.openai.com/v1` | yes |
| `anthropic` | Anthropic Messages | `https://api.anthropic.com` | yes |
| `ollama` | Ollama native API | `http://127.0.0.1:11434` | no (a stored key is sent as a bearer token: Ollama's hosted API) |
| `openai-compatible` | `/chat/completions` + `/models` | none: `--base-url` is required | no (a stored key is sent when there is one) |

OpenAI-style base URLs include the API version, the convention of OpenAI's SDKs: `https://openrouter.ai/api/v1`, `https://api.groq.com/openai/v1`, `http://127.0.0.1:1234/v1` (LM Studio), a vLLM or llama.cpp server's `/v1`. A base URL must be http(s), carry no user name, password, query or fragment (a secret never belongs in it), and is stored without a trailing slash.

### The adapters

- **OpenAI-compatible** (`openai.rs`): `POST {base}/chat/completions` with `stream: true` and `stream_options.include_usage`, SSE `data:` lines, ends at `data: [DONE]` (a server that only sends `finish_reason` and closes is accepted). Deltas are `choices[0].delta.content`; `reasoning_content` deltas some servers add are not shown. Usage comes from the final chunk (`prompt_tokens`, `completion_tokens`, `total_tokens`, `prompt_tokens_details.cached_tokens`, `completion_tokens_details.reasoning_tokens`). `max_tokens` is sent as `max_completion_tokens` to `openai` (its newer models reject the old name) and as `max_tokens` to compatible servers. `GET {base}/models` → `data[].id`.
- **Anthropic** (`anthropic.rs`): `POST {base}/v1/messages` with `x-api-key` and `anthropic-version: 2023-06-01`, SSE events by their JSON `type`: `message_start` (input and cache-read tokens), `content_block_delta` (`text_delta` only), `message_delta` (`stop_reason`, output tokens), `message_stop`, `error`; `ping` and other kinds are ignored. `system` messages become the top-level `system` field; `max_tokens` is required by the API, so 8192 is used when a request names none. `GET {base}/v1/models`.
- **Ollama** (`ollama.rs`): the **native** API, `POST {base}/api/chat` (newline-delimited JSON) and `GET {base}/api/tags`. Chosen over Ollama's OpenAI-compatible `/v1` endpoint because the native API reports `prompt_eval_count`/`eval_count` and `done_reason` on every reply, answers an unknown model with a plain 404 and `model 'x' not found`, takes `num_predict`/`temperature` as documented options, and exists in every Ollama version, while the compatibility layer has changed between releases. Someone who prefers the compatible endpoint adds Ollama as `openai-compatible` with base URL `http://127.0.0.1:11434/v1`.

Every adapter turns a reply into `GatewayEvent`s: any number of `Delta`s, at most one `Usage` (a number the provider did not report is absent, never zero), then `Completed { finish_reason }`; a failure is an `Err` instead, and cancelling ends the stream with `Err(Cancelled)` after closing the connection. The daemon maps `Delta` → `message_delta`, `Usage` → `usage`, `Completed` → `turn_completed`, errors → `turn_failed`, cancellation → `turn_cancelled`.

### HTTP behaviour

`reqwest` with rustls (`ring` provider, the macOS verifier for trust roots), redirects off (a redirect could carry a vendor header such as `x-api-key` to another host: a 3xx is an error). Timeouts (`HttpConfig`): connect 10 s, 120 s of silence while waiting for the reply or between two chunks of a stream (a model that thinks first needs it to be generous), 30 s for a whole model-list request.

**Retry: only connect errors, only before any byte of the request was sent** — up to two more attempts, 250 ms · n apart. A request the provider may have seen is never repeated: an HTTP error status, a connection that breaks after the request left, and a stream that breaks mid-reply all fail at once. `Connect` errors say how many attempts were made.

| Status | Error | Shown as |
|---|---|---|
| 401, 403 | `Unauthorized` | `the provider refused the credentials (HTTP 401): <provider's message>` |
| 404 | `NotFound` | `not found: <message>; check the model name and the provider's base URL` |
| 429 | `RateLimited` | `rate limited (retry after 7s): <message>`; `retry_after_secs` is also in the protocol error's `data` |
| 5xx (529 included) | `Server` | `the provider failed (HTTP 503): <message>` |
| other 4xx, 3xx | `Rejected` | `the provider rejected the request (HTTP 400): <message>` |
| no connection | `Connect` | `could not connect to the provider after 3 attempt(s): …` |
| silence | `Timeout` | `the provider did not answer in time` |
| broken or failing stream | `Stream` | `the stream failed: <message>` |

The provider's message is `error.message`, a string `error`, or `message` of its JSON body, else the raw text, cut at 400 characters.

### Local discovery

`provider.discover` (`hutan provider discover [--add]`) asks `127.0.0.1:11434/api/tags` (Ollama) and `127.0.0.1:1234/v1/models` (LM Studio) in parallel, once per request, with a 0.75 s connect timeout and no retries, ignoring proxy settings. It never runs on a timer or at startup. A server that answers is reported with its model ids and the configured provider already at that URL, if any.

## Credentials (ADR 0008)

- `provider.set_credential { provider_id, secret }` writes a generic-password item to the macOS Keychain: service `dev.hutan.provider`, account = the provider id (`security-framework`). SQLite stores `keychain_ref` = `dev.hutan.provider/<id>` and nothing else; `provider.list` says `has_credential: bool`. `provider.clear_credential` and `provider.remove` delete the item.
- The secret is never echoed, logged, journaled or stored elsewhere. `Secret` has no `Display`, no `Serialize` and a redacted `Debug`; the protocol request that carries it prints `<redacted>`; the key travels in a header marked sensitive and is never part of a URL; HTTP logging is one `debug!` line per request and per status (no headers, no bodies). Provider error text goes through a `Redactor` that removes the exact key and key-shaped strings (`sk-…`, `Bearer …`) before it can become a `turn_failed` message — providers echo masked keys in their 401s.
- The daemon reads the key when it builds the adapter for a turn (or a `provider.models`), so a changed key takes effect at once and a Keychain prompt, if the daemon's identity changed, happens on use (ADR 0007: sign the daemon with a stable identity).
- The credential store is a trait: the default gate uses `MemoryStore`; `KeychainStore` is the daemon's. One `#[ignore]`d test creates a temporary keychain file with `security create-keychain` in a temp directory, round-trips a credential through `KeychainStore::open_file`, checks it is invisible to the default search list, and deletes the keychain (`cargo test -p hutan-gateway --test credentials -- --ignored`). Nothing ever touches the login keychain.
- Subscription logins are not providers: "use my ChatGPT plan" is a Codex chat (`docs/agents.md`), never a token reused here.

## Direct chat sessions

`chat.create { agent: "direct", provider_id, model?, system_prompt?, cwd }` creates a session of kind `harness` with `SessionInfo.chat = { agent: "direct", provider_id, model, … }`; the provider must exist, and the model is the request's or the provider's `default_model`. There is no process: the session is `idle` at once.

A turn (`DirectSession::run_turn`), in order:

1. `chat.send` resolves the provider (and its key) — failing the call itself, before anything is journaled, when the provider is gone or lacks a required key — and builds the request from the **journal**: the `message_end` events of the session (user and assistant text only), oldest first, plus the system prompt and the new message. **Truncation:** at most `gateway.max_context_chars` (default 200 000) characters of conversation are sent; the newest messages that fit are kept, the oldest are left out, a kept conversation never starts with an assistant message, the new message is always sent, the system prompt is not counted. The turn gets a `notice` (`context truncated: the N oldest message(s) were left out (gateway.max_context_chars = …)`). The journal itself is never shortened.
2. `turn_started` (payload `{provider_id, model}`), the user's `message_start`/`message_end`, the truncation notice if any.
3. The streamed reply: `message_start` at the first delta (so a failure before any text leaves no empty assistant message), `message_delta`s, then `message_end` with the whole text, `usage` (`last` = this request, `total` = the conversation's running sum, continued from the journal after a restart), a `notice` when the provider says the reply was cut at the token limit, and exactly one of `turn_completed`, `turn_failed`, `turn_cancelled`.
4. **Interrupt** (`chat.interrupt`) cancels the stream: the HTTP connection is closed, the text so far is kept as the assistant's `message_end`, the turn ends `turn_cancelled`. A failed or cancelled turn leaves its messages in the conversation (retrying sends them again).

**Switching models** (`chat.set_model`, `hutan chat model`) changes provider and/or model for the next turn; a turn in progress keeps what it started with. The session's spec and info change at once (and survive a restart), the journal gets `notice` `model switched from a / m1 to b / m2`, and the history is sent to the new provider through its own API. A switch to what is already set does nothing. Only a running chat can be switched (`hutan restart` a stopped one first).

Restart: a live direct chat is started again with the same session id (`restore_sessions = "respawn"`), its journal and model intact; `restore_sessions = "stopped"`, `session.stop` and `session.restart` work as for Codex chats. A chat whose provider was removed stays listed; its next `chat.send` says so, and `hutan chat model <session> <model> --provider <other>` repairs it.

## Images

Protocol 0.23. A user message may carry images (`ChatMessage.images`): OpenAI-compatible `image_url` data URLs, Anthropic `image` base64 blocks (before the text), Ollama `images`. Before sending, the daemon asks `Provider::supports_vision(model)`: OpenRouter-style `architecture.input_modalities`, a `vision` capability or LM Studio `type: "vlm"` in `/models` (OpenAI's own models by family, since its list says nothing), `capabilities.image_input` of Anthropic's `/v1/models`, `capabilities` of Ollama's `/api/show`. A model the provider does not say reads images is refused with `unsupported` (the answer is cached for five minutes). Later turns carry the image headers from the journal, not the images.

## Configuration

`daemon.toml`:

```toml
[gateway]
max_context_chars = 200000     # characters of conversation per request; the oldest are left out
```

Providers, keys, and models are not configuration files: use `hutan provider …`.

## Desktop

Settings → **models** (or ⌘K → **Models: Manage Providers…**) manages provider configuration
through `hutand`: add/edit endpoint and default model, remove with confirmation, list/refresh
models, discover local Ollama/LM Studio, and set/clear credentials. The key prompt is masked,
supports paste, redacts Debug output and moves its input into one IPC request on submit.
Neither the key nor provider secrets are saved in desktop state or settings files.

⌘⌥T creates a direct chat; the Start… sheet (⌘⇧T) also offers configured direct providers next to the harnesses.
Type to filter the model list, choose the default or another model, and optionally set a
system prompt. Direct chats reuse the streaming transcript and usage footer. The header's
**Switch model…** control (or ⌘K → **Chat: Switch Model…**) changes the next turn's provider
and model; switching is disabled during a turn. The journal displays the switch notice.
Missing/rejected keys offer **Set key…**; missing models offer **Pick another model…**.
Other gateway errors remain inline, including the provider's retry-after hint for rate limits. A failed turn that may pass (rate limit, provider failure, connection, timeout, broken stream) gets a recovery row under its footer in the desktop (`UX-RETRY`): one sentence on what to do next, `Retry available in N s` counted down from a `retry-after` until its window passes, and **Retry now**, which sends the conversation's last message again (`chat.send`); it is offered on the newest failure only, while no turn runs and nothing was sent since. Known gap: that resend is a second `message_end` of the user in the journal and the provider sees it twice; a retry that reuses the failed turn's message needs a daemon/protocol request.

## CLI

```text
hutan provider add openai|anthropic|ollama|openai-compatible [--name N] [--base-url URL] [--model M]
hutan provider key <id> [--from-env VAR | --clear]   # secret from stdin (hidden prompt on a terminal), never an argument
hutan provider ls | models <id> | rm <id>
hutan provider discover [--add]
hutan chat new --direct --provider <id> [--model M] [--system TEXT] [--cwd DIR] [--ws W] [--title T]
hutan chat model <session> <model> [--provider <id>]
hutan chat send|log|attach|interrupt …               # as for agent chats
```

`--json` prints the protocol result. A key is never a command-line argument (it would be visible in `ps` and shell history): it is read from stdin — without echo on a terminal — or from an environment variable.

## Tests

Real providers are never contacted. Adapter tests (`crates/gateway/tests/it/adapters.rs`) run against `hutan_gateway::testing::MockServer`, a hand-written HTTP/1.1 server on a loopback port that serves the byte shapes the providers document (SSE and NDJSON written from their public docs), pauses, holds a connection open, drops it mid-body, and records every request: streaming, usage, error mapping, 429 `Retry-After`, cancellation before and during the stream (the server sees the client hang up), connect retry only before the first byte, no retry of HTTP errors or broken streams, missing-key refusal, redirect refusal, and a log scan (no key, no prompt text at TRACE). Daemon tests (`crates/daemon/tests/it/direct.rs`) run a real daemon with `MemoryStore`: provider CRUD across restarts, validation, models, discovery on request only, direct chat create → send → stream → journal → restart → history and context, truncation, model switching, interrupt, stop mid-turn, provider failures, and a scan of every response, event, file of the daemon's home (database and WAL included) and every log line for the secret. CLI tests (`crates/cli/tests/it/provider.rs`) drive the real `hutan` binary. Protocol fixtures: `crates/protocol/tests/fixtures/v0/*provider*`, `*chat_set_model*`, `*direct*`.

**Persistence errors (protocol 0.28).** `provider.add`, `.update` and `.remove` write the provider's row before they answer. When SQLite refuses the write they answer `internal` and the in-memory list is what it was before the request (a removal has already deleted the key by then, as before: it comes back as a provider without one). Before 0.28 the failure was only logged and the answer said success, so a provider could vanish at the next start.

## Limits

Text only (no images or files, no tools), no reasoning or thinking display, no Gemini or other native APIs (Gemini's OpenAI-compatible endpoint works as `openai-compatible`), no OAuth flows, no per-request settings beyond `gateway.max_context_chars` (temperature and max tokens exist in the provider layer but are not exposed yet), no cost accounting. Keys are not shared with agents the daemon runs.
