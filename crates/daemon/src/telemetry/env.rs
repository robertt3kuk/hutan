//! The per-session capability of the OTLP receiver (ADR 0026 decision 3) and the environment a
//! terminal gets while the receiver listens (decision 8).
//!
//! A token is 128 random bits from the operating system, held **in memory only**: never in
//! SQLite, events, logs, argv, notifications or `hutan ps`. A terminal that starts gets a new
//! one (a restart or a reopened session replaces the old); a closed session loses it. The token
//! *selects the session*: nothing inside a payload does.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hutan_core::SessionId;
use subtle::ConstantTimeEq as _;

/// `HUTAN_OTLP_ENDPOINT`: `http://127.0.0.1:<port>/s/<token>`, the base an exporter appends
/// `/v1/<signal>` to. Non-standard names: nothing reads them by accident.
pub const ENDPOINT_ENV: &str = "HUTAN_OTLP_ENDPOINT";
/// `HUTAN_OTLP_TOKEN`: the same token alone, for an agent whose endpoint is fixed in a config
/// file and takes the token as a header (`x-hutan-token`).
pub const TOKEN_ENV: &str = "HUTAN_OTLP_TOKEN";

/// Hex characters of a token (128 bits).
const TOKEN_LEN: usize = 32;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // No critical section leaves the state half updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Default)]
struct Inner {
    /// The port the receiver listens on; `None` while it does not.
    listening: Option<u16>,
    tokens: HashMap<SessionId, String>,
}

/// The receiver's tokens, shared between the session registry (which issues them) and the
/// receiver (which resolves them).
#[derive(Debug, Default)]
pub struct SessionEnv {
    inner: Mutex<Inner>,
}

impl SessionEnv {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The environment of a terminal that starts now: the endpoint and the session's new token,
    /// or nothing while the receiver does not listen (off, or its port is busy). A new token
    /// replaces the session's old one.
    pub fn issue(&self, session: SessionId) -> Vec<(String, String)> {
        let mut inner = lock(&self.inner);
        let Some(port) = inner.listening else {
            // A process that starts while the receiver is off has no way to report; the token
            // of the one before it is not left behind to be used.
            inner.tokens.remove(&session);
            return Vec::new();
        };
        let Some(token) = new_token() else {
            tracing::warn!("no random bytes for an OTLP token; this session gets none");
            inner.tokens.remove(&session);
            return Vec::new();
        };
        inner.tokens.insert(session, token.clone());
        vec![
            (
                ENDPOINT_ENV.to_owned(),
                format!("http://127.0.0.1:{port}/s/{token}"),
            ),
            (TOKEN_ENV.to_owned(), token),
        ]
    }

    /// The session is closed: its token is no longer valid.
    pub fn revoke(&self, session: SessionId) {
        lock(&self.inner).tokens.remove(&session);
    }

    /// The session a presented token belongs to. Compares every token in constant time (a
    /// wrong token costs the same whichever prefix it shares); `None` for any failure.
    pub fn resolve(&self, presented: &str) -> Option<SessionId> {
        let inner = lock(&self.inner);
        let presented = presented.as_bytes();
        let mut found = None;
        for (session, token) in &inner.tokens {
            // The length of a token is not a secret, its bytes are.
            let same =
                presented.len() == token.len() && bool::from(presented.ct_eq(token.as_bytes()));
            if same {
                found = Some(*session);
            }
        }
        found
    }

    /// Whether the session has a token (it started a process while the receiver listened).
    pub fn has_token(&self, session: SessionId) -> bool {
        lock(&self.inner).tokens.contains_key(&session)
    }

    /// The receiver listens on `port` now, or no longer (`None`).
    pub(crate) fn set_listening(&self, port: Option<u16>) {
        lock(&self.inner).listening = port;
    }

    /// Sessions that hold a token.
    #[cfg(test)]
    pub(crate) fn token_count(&self) -> usize {
        lock(&self.inner).tokens.len()
    }
}

/// 128 random bits as 32 lowercase hex characters; `None` when the system has none to give.
fn new_token() -> Option<String> {
    let mut bytes = [0_u8; TOKEN_LEN / 2];
    getrandom::fill(&mut bytes).ok()?;
    let mut token = String::with_capacity(TOKEN_LEN);
    for byte in bytes {
        token.push(char::from_digit(u32::from(byte >> 4), 16)?);
        token.push(char::from_digit(u32::from(byte & 0xf), 16)?);
    }
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_environment_and_no_token_while_the_receiver_does_not_listen() {
        let env = SessionEnv::new();
        let session = SessionId::new();
        assert!(env.issue(session).is_empty());
        assert_eq!(env.token_count(), 0);
    }

    #[test]
    fn a_token_selects_its_session_and_only_it() {
        let env = SessionEnv::new();
        env.set_listening(Some(51234));
        let (a, b) = (SessionId::new(), SessionId::new());
        let vars_a = env.issue(a);
        let vars_b = env.issue(b);
        let token = |vars: &[(String, String)]| {
            vars.iter()
                .find(|(k, _)| k == TOKEN_ENV)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        let (ta, tb) = (token(&vars_a), token(&vars_b));
        assert_eq!(ta.len(), TOKEN_LEN);
        assert!(ta.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(ta, tb);
        assert_eq!(env.resolve(&ta), Some(a));
        assert_eq!(env.resolve(&tb), Some(b));
        assert_eq!(env.resolve(""), None);
        assert_eq!(env.resolve(&ta[..TOKEN_LEN - 1]), None);
        assert_eq!(env.resolve(&format!("{ta}0")), None);
        // The endpoint carries the same token in its path.
        let endpoint = vars_a.iter().find(|(k, _)| k == ENDPOINT_ENV).unwrap();
        assert_eq!(endpoint.1, format!("http://127.0.0.1:51234/s/{ta}"));
    }

    #[test]
    fn a_new_process_gets_a_new_token_and_a_closed_session_none() {
        let env = SessionEnv::new();
        env.set_listening(Some(1));
        let session = SessionId::new();
        let first = env.issue(session)[1].1.clone();
        let second = env.issue(session)[1].1.clone();
        assert_ne!(first, second);
        assert_eq!(env.resolve(&first), None, "the old process's token is dead");
        assert_eq!(env.resolve(&second), Some(session));
        env.revoke(session);
        assert_eq!(env.resolve(&second), None);
        // A start while the receiver is off leaves the session with no token at all.
        env.issue(session);
        env.set_listening(None);
        assert!(env.issue(session).is_empty());
        assert!(!env.has_token(session));
    }
}
