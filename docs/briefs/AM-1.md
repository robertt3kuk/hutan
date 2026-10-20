# AM-1 — the assistant picks its backend and model from what is installed

Owner: implementer. Crates: `daemon` (main), `desktop` (setup default, panel label), docs. No protocol change.

## Problem

1. With nothing configured (`assistant.open` without a stored `assistant.configure` and without `[assistant]` in `daemon.toml`) the daemon fails with "no assistant backend is configured", although a logged-in harness or a usable provider is right there.
2. With a backend but no model (the setup stores `backend: omp, model: None`), the harness's own default runs. For OMP that is the user's OMP default, here **Claude Opus** — the most expensive tier, for a read-only helper that answers questions. The user wants the assistant to choose by what is installed: which harness is there and logged in, and which providers that harness can use.

## Change

### Daemon (`crates/daemon/src/assistant.rs`, new `crates/daemon/src/assistant_pick.rs` or a module next to it)

1. **Automatic backend.** `effective()` keeps its order (workspace → parent → global → `daemon.toml`). When all are empty it returns the automatic backend: the first *ready* one in this order:
   - `omp` — OMP found and at least one provider logged in (`AccountService` / `omp_catalog` non-empty);
   - `claude` — Claude Code logged in **and** its ACP adapter found (`Harnesses::acp_config("claude")` succeeds);
   - `codex` — Codex logged in;
   - `provider` — the first provider (`hutan provider ls` order) that has a credential, or is a local kind that needs none (Ollama, LM Studio, other `openai-compatible` on loopback); model = its `default_model`, else the pick below over `provider.models`.
   None ready → the existing "no assistant backend" error, now listing what was checked (one short reason per candidate).
   Readiness is computed lazily on `assistant.open`/`assistant.list`, cached for a short TTL (≤ 60 s, bounded, invalidated on `account.*` login/logout and `provider.*` changes if cheap); it must never run on the request path more than once per TTL. `AssistantEntry.backend` already reports the effective backend — keep it truthful for the automatic one.
2. **Automatic model.** When the effective backend has no model (stored, `daemon.toml`, or automatic), the daemon picks one from the backend's catalog (`ChatService::models(agent)`; `provider.models` for providers) with a pure function `pick_assistant_model(&[ModelInfo]) -> Option<String>`:
   - Take the catalog default (`is_default`), else the first non-hidden model.
   - If that model is a **premium tier**, replace it by the **newest model of the same vendor one tier down**, when the catalog has one: Anthropic `opus` → newest `sonnet`; OpenAI `*-pro` (incl. `o3-pro`, `gpt-5-pro`) → the same family without `-pro`; Google `*-ultra`/`deep-think` → `*-pro`. Match on `id`, `display_name` and `description` (case-insensitive) — Claude Code's ACP adapter exposes ids like `default`/`opus`/`sonnet`/`haiku` with the real model in the description; OMP ids are `provider/model`, keep the provider prefix when choosing the replacement (an `anthropic/…opus…` default becomes `anthropic/…sonnet…`, never another provider's model).
   - "Newest": highest version found in the id (`4-5` > `4-1` > `4`; dates `20250929` > `20250514`), ties → catalog order.
   - Otherwise keep the default. `None` (empty catalog, or the catalog cannot be read) → no model, harness default, as today — never fail the open because of the pick.
   The picked id goes into `ChatCreate.model`, so the session's `chat.model` shows what runs. A stored/configured model always wins over the pick. The pick is a fixed table in code (comment why each rule exists); no network, no paid calls.
3. Errors and timeouts: the catalog read uses the existing cached `models()` (it may spawn the harness once); a slow or failing catalog → no model (log at debug), the open continues.

### Desktop

- `Workbench::setup_backend` (`crates/desktop/src/workbench/setup.rs`): the default is the first ready harness in the same order (OMP, Claude Code with adapter, Codex) from the harness directory the desktop already has, not OMP only.
- Assistant panel backend/model chooser (`panels/assistant.rs`): where the model is unset show "Automatic" and, once the session exists, the model it runs (`session.chat.model`). Choosing "Use default" keeps meaning "no stored model" (the daemon picks).
- No new settings.

### Docs

`docs/assistant.md` § Backends: the automatic backend order, readiness, and the model rule with the premium table; `daemon.toml` section unchanged.

## Tests (deterministic, no real harness or network)

- Unit tests for `pick_assistant_model` over realistic catalogs: an OMP catalog (several providers, `anthropic/claude-opus-4-1` default → `anthropic/claude-sonnet-4-5`; versions and dates ordering; never cross provider), a Claude Code ACP catalog (`default` described as Opus → `sonnet`), a Codex catalog (`gpt-5-codex` default kept; a `-pro` default → non-pro), a catalog with no lower tier (keeps default), an empty catalog (`None`), hidden models skipped.
- Daemon integration test (existing mock harness / fake omp in `crates/daemon/tests/it/assistant_backend.rs`): nothing configured + a ready fake OMP whose catalog defaults to an opus model → `assistant.open` succeeds, `backend == omp`, `chat.model` is the sonnet id; nothing ready → the error names the checked candidates; a stored model is used as-is (no pick).
- Desktop: `setup_gpui` — with OMP missing and Codex logged in the Agents step preselects Codex.

## Acceptance

- `just check` green.
- Behaviour above, proved by the tests. No protocol change, no golden changes.
- Do not edit `core`/`protocol` (report if you need to), `docs/status.md`, `docs/roadmap.md`.
