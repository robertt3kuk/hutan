//! The model gateway (ADR 0008, `docs/gateway.md`): direct chat with a model provider, without
//! an agent in between.
//!
//! - [`Provider`]: `list_models()` and `stream_chat()`, implemented for the OpenAI-compatible API
//!   (`/chat/completions`), Anthropic's Messages API and Ollama's native API, over one
//!   [`Http`] client (timeouts, retry of connect errors only, status mapping).
//! - [`CredentialStore`]: where API keys live: the macOS Keychain ([`KeychainStore`]), or memory
//!   for tests ([`MemoryStore`]). [`Secret`] never prints.
//! - [`discovery`]: asking the loopback ports of Ollama and LM Studio what they serve.
//!
//! Nothing here knows about sessions, storage or the wire protocol: the daemon owns those and
//! maps [`GatewayEvent`]s onto `AgentEvent`s.

mod anthropic;
pub mod credentials;
pub mod discovery;
pub mod error;
mod framing;
pub mod http;
mod ollama;
mod openai;
pub mod provider;
pub mod redact;
pub mod secret;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod types;

#[cfg(target_os = "macos")]
pub use credentials::KeychainStore;
pub use credentials::{
    CredentialError, CredentialStore, KEYCHAIN_SERVICE, MemoryStore, keychain_ref, system_store,
};
pub use discovery::{Discovered, ProbeTarget};
pub use error::GatewayError;
pub use http::{Fetched, Http, HttpConfig, validate_base_url};
pub use provider::{Provider, ProviderConfig, connect};
pub use redact::Redactor;
pub use secret::Secret;
pub use types::{
    ChatMessage, ChatRequest, GatewayEvent, GatewayStream, ImageContent, ProviderKind, Role,
    is_length_cut,
};
