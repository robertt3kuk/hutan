//! Model providers (protocol 0.11, `docs/gateway.md`): their configuration in SQLite, their API
//! keys in the Keychain (ADR 0008), and the adapters that talk to them.
//!
//! A secret enters through [`ProviderService::set_credential`], goes straight to the
//! [`CredentialStore`], and is read back only to build the adapter of one request. It is never
//! logged, never in a response or event, never in the database.

use std::collections::HashSet;
use std::sync::Arc;

use hutan_gateway::{
    CredentialError, CredentialStore, GatewayError, Http, ProbeTarget, Provider, ProviderConfig,
    ProviderKind, Secret, connect, keychain_ref, validate_base_url,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::provider::{
    DiscoveredProvider, ProviderAdd, ProviderDiscovery, ProviderInfo, ProviderList, ProviderModels,
    ProviderResult, ProviderUpdate,
};
use hutan_storage::{ProviderRow, Storage};
use parking_lot::Mutex;
use tracing::{info, warn};

use crate::registry;

/// Most providers one daemon keeps.
const MAX_PROVIDERS: usize = 64;
/// Longest accepted provider name, model name and API key, in bytes.
const MAX_NAME_BYTES: usize = 120;
const MAX_MODEL_BYTES: usize = 200;
const MAX_SECRET_BYTES: usize = 16 * 1024;

/// A provider as the daemon holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderRecord {
    pub(crate) id: String,
    pub(crate) kind: ProviderKind,
    pub(crate) name: String,
    pub(crate) base_url: String,
    pub(crate) default_model: Option<String>,
    pub(crate) created_at_ms: u64,
}

/// How the daemon reaches model providers; `DaemonConfig::gateway`.
#[derive(Debug, Clone, Default)]
pub struct GatewayConfig {
    /// Where API keys live; `None` is the macOS Keychain.
    pub credentials: Option<Arc<dyn CredentialStore>>,
    /// Timeouts and retry policy of the HTTP client.
    pub http: hutan_gateway::HttpConfig,
    /// Where `provider.discover` looks; `None` is Ollama's and LM Studio's default ports.
    pub discovery: Option<Vec<ProbeTarget>>,
}

#[derive(Debug)]
pub struct ProviderService {
    storage: Storage,
    store: Arc<dyn CredentialStore>,
    http: Http,
    discovery: Vec<ProbeTarget>,
    /// Oldest first; mirrors the `providers` table.
    providers: Mutex<Vec<ProviderRecord>>,
    /// Providers known to have a stored key; learned from the Keychain on first sight and kept
    /// current by `set_credential` / `clear_credential` / `remove`.
    credentialed: Mutex<HashSet<String>>,
    checked: Mutex<HashSet<String>>,
}

fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

fn internal(context: &str, error: impl std::fmt::Display) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, format!("{context}: {error}"))
}

/// A provider failure as a protocol error: `provider_error` with the gateway's message (which
/// never holds a credential) and, for a 429, `data.retry_after_secs`.
pub(crate) fn gateway_error(error: &GatewayError) -> ErrorBody {
    let mut body = ErrorBody::new(ErrorCode::ProviderError, error.to_string());
    if let Some(after) = error.retry_after() {
        body.data = Some(serde_json::json!({ "retry_after_secs": after.as_secs().max(1) }));
    }
    body
}

fn credential_error(error: &CredentialError) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, error.to_string())
}

impl ProviderService {
    pub(crate) fn new(
        storage: Storage,
        store: Arc<dyn CredentialStore>,
        http: Http,
        discovery: Vec<ProbeTarget>,
        rows: Vec<ProviderRow>,
    ) -> Arc<Self> {
        let providers = rows
            .into_iter()
            .filter_map(|row| match ProviderKind::parse(&row.kind) {
                Some(kind) => Some(ProviderRecord {
                    id: row.id,
                    kind,
                    name: row.name,
                    base_url: row.base_url,
                    default_model: row.default_model,
                    created_at_ms: row.created_at_ms,
                }),
                None => {
                    warn!(provider = %row.id, kind = %row.kind, "stored provider has an unknown kind; ignoring it");
                    None
                }
            })
            .collect();
        Arc::new(Self {
            storage,
            store,
            http,
            discovery,
            providers: Mutex::new(providers),
            credentialed: Mutex::new(HashSet::new()),
            checked: Mutex::new(HashSet::new()),
        })
    }

    // Lookups -----------------------------------------------------------------------------------

    pub(crate) fn record(&self, id: &str) -> Result<ProviderRecord, ErrorBody> {
        self.providers
            .lock()
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("provider `{id}` not found (see `hutan provider ls`)"),
                )
            })
    }

    /// Like [`record`](Self::record) but a `bad_request`: for a chat naming a provider.
    pub(crate) fn require(&self, id: &str) -> Result<ProviderRecord, ErrorBody> {
        self.record(id).map_err(|mut e| {
            e.code = ErrorCode::BadRequest;
            e
        })
    }

    // Credentials -------------------------------------------------------------------------------

    async fn has_credential(self: &Arc<Self>, id: &str) -> Result<bool, ErrorBody> {
        if self.checked.lock().contains(id) {
            return Ok(self.credentialed.lock().contains(id));
        }
        let this = self.clone();
        let account = id.to_owned();
        let found = tokio::task::spawn_blocking(move || this.store.get(&account))
            .await
            .map_err(|e| internal("reading the credential", e))?
            .map_err(|e| credential_error(&e))?
            .is_some();
        self.note_credential(id, found);
        Ok(found)
    }

    fn note_credential(&self, id: &str, present: bool) {
        self.checked.lock().insert(id.to_owned());
        let mut credentialed = self.credentialed.lock();
        if present {
            credentialed.insert(id.to_owned());
        } else {
            credentialed.remove(id);
        }
    }

    async fn info(self: &Arc<Self>, record: &ProviderRecord) -> Result<ProviderInfo, ErrorBody> {
        // A Keychain that cannot be read does not hide the provider: it shows as without key.
        let has_credential = match self.has_credential(&record.id).await {
            Ok(has) => has,
            Err(error) => {
                warn!(provider = %record.id, message = %error.message, "could not tell whether a credential is stored");
                false
            }
        };
        Ok(ProviderInfo {
            id: record.id.clone(),
            kind: record.kind.as_str().to_owned(),
            name: record.name.clone(),
            base_url: record.base_url.clone(),
            default_model: record.default_model.clone(),
            has_credential,
            created_at_ms: record.created_at_ms,
        })
    }

    /// Stores the API key of a provider in the credential store.
    pub async fn set_credential(
        self: &Arc<Self>,
        provider_id: &str,
        secret: String,
    ) -> Result<ProviderResult, ErrorBody> {
        let record = self.record(provider_id)?;
        let secret = secret.trim();
        if secret.is_empty() {
            return Err(bad_request("the secret is empty"));
        }
        if secret.len() > MAX_SECRET_BYTES {
            return Err(bad_request(format!(
                "the secret is longer than {MAX_SECRET_BYTES} bytes"
            )));
        }
        if secret.chars().any(char::is_control) {
            return Err(bad_request(
                "the secret contains a control character (a pasted newline?)",
            ));
        }
        let this = self.clone();
        let account = record.id.clone();
        let secret = Secret::new(secret);
        tokio::task::spawn_blocking(move || this.store.set(&account, &secret))
            .await
            .map_err(|e| internal("storing the credential", e))?
            .map_err(|e| credential_error(&e))?;
        self.note_credential(&record.id, true);
        info!(provider = %record.id, "credential stored");
        Ok(ProviderResult {
            provider: self.info(&record).await?,
        })
    }

    pub async fn clear_credential(
        self: &Arc<Self>,
        provider_id: &str,
    ) -> Result<ProviderResult, ErrorBody> {
        let record = self.record(provider_id)?;
        self.delete_credential(&record.id).await?;
        info!(provider = %record.id, "credential removed");
        Ok(ProviderResult {
            provider: self.info(&record).await?,
        })
    }

    async fn delete_credential(self: &Arc<Self>, id: &str) -> Result<(), ErrorBody> {
        let this = self.clone();
        let account = id.to_owned();
        tokio::task::spawn_blocking(move || this.store.delete(&account))
            .await
            .map_err(|e| internal("removing the credential", e))?
            .map_err(|e| credential_error(&e))?;
        self.note_credential(id, false);
        Ok(())
    }

    // CRUD --------------------------------------------------------------------------------------

    pub async fn list(self: &Arc<Self>) -> Result<ProviderList, ErrorBody> {
        let records = self.providers.lock().clone();
        let mut providers = Vec::with_capacity(records.len());
        for record in &records {
            providers.push(self.info(record).await?);
        }
        Ok(ProviderList { providers })
    }

    pub async fn add(self: &Arc<Self>, req: ProviderAdd) -> Result<ProviderResult, ErrorBody> {
        let kind = ProviderKind::parse(&req.kind).ok_or_else(|| {
            bad_request(format!(
                "unknown provider kind `{}` (one of: {})",
                req.kind,
                ProviderKind::ALL
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        let name = checked_name(&req.name)?;
        let base_url = match (req.base_url.as_deref(), kind.default_base_url()) {
            (Some(url), _) => validate_base_url(url).map_err(|e| bad_request(e.to_string()))?,
            (None, Some(default)) => default.to_owned(),
            (None, None) => {
                return Err(bad_request(format!(
                    "a `{kind}` provider needs a base URL (e.g. http://127.0.0.1:1234/v1)"
                )));
            }
        };
        let default_model = checked_model(req.default_model.as_deref())?;
        let record = {
            let mut providers = self.providers.lock();
            if providers.len() >= MAX_PROVIDERS {
                return Err(ErrorBody::new(
                    ErrorCode::Conflict,
                    format!("at most {MAX_PROVIDERS} providers can be configured"),
                ));
            }
            let id = unique_id(&providers, &slug(&name, kind));
            let record = ProviderRecord {
                id,
                kind,
                name,
                base_url,
                default_model,
                created_at_ms: registry::now_ms(),
            };
            providers.push(record.clone());
            record
        };
        if let Err(error) = self.persist(&record).await {
            self.providers.lock().retain(|p| p.id != record.id);
            return Err(error);
        }
        info!(provider = %record.id, kind = %record.kind, "provider added");
        Ok(ProviderResult {
            provider: self.info(&record).await?,
        })
    }

    pub async fn update(
        self: &Arc<Self>,
        req: ProviderUpdate,
    ) -> Result<ProviderResult, ErrorBody> {
        let name = req.name.as_deref().map(checked_name).transpose()?;
        let base_url = req
            .base_url
            .as_deref()
            .map(|url| validate_base_url(url).map_err(|e| bad_request(e.to_string())))
            .transpose()?;
        let default_model = req
            .default_model
            .as_deref()
            .map(|m| checked_model(Some(m)))
            .transpose()?;
        let (record, previous) = {
            let mut providers = self.providers.lock();
            let record = providers
                .iter_mut()
                .find(|p| p.id == req.provider_id)
                .ok_or_else(|| {
                    ErrorBody::new(
                        ErrorCode::NotFound,
                        format!("provider `{}` not found", req.provider_id),
                    )
                })?;
            let previous = record.clone();
            if let Some(name) = name {
                record.name = name;
            }
            if let Some(url) = base_url {
                record.base_url = url;
            }
            if let Some(model) = default_model {
                record.default_model = model;
            }
            (record.clone(), previous)
        };
        if let Err(error) = self.persist(&record).await {
            if let Some(current) = self
                .providers
                .lock()
                .iter_mut()
                .find(|p| p.id == previous.id)
            {
                *current = previous;
            }
            return Err(error);
        }
        Ok(ProviderResult {
            provider: self.info(&record).await?,
        })
    }

    /// Removes a provider and its key. The key goes first: a failure there leaves the provider
    /// (and the key) in place to retry. When the database then refuses the delete, the provider
    /// is back in the list (without its key) and the error says so.
    pub async fn remove(self: &Arc<Self>, provider_id: &str) -> Result<(), ErrorBody> {
        let record = self.record(provider_id)?;
        self.delete_credential(&record.id).await?;
        let index = {
            let mut providers = self.providers.lock();
            let index = providers.iter().position(|p| p.id == record.id);
            if let Some(index) = index {
                providers.remove(index);
            }
            index
        };
        let storage = self.storage.clone();
        let id = record.id.clone();
        let stored = tokio::task::spawn_blocking(move || storage.providers().delete(&id))
            .await
            .map_err(|e| internal("removing the provider", e))
            .and_then(|result| result.map_err(|e| internal("removing the provider", e)));
        if let Err(error) = stored {
            let mut providers = self.providers.lock();
            if !providers.iter().any(|p| p.id == record.id) {
                let at = index.map_or(providers.len(), |i| i.min(providers.len()));
                providers.insert(at, record);
            }
            return Err(error);
        }
        self.checked.lock().remove(&record.id);
        info!(provider = %record.id, "provider removed");
        Ok(())
    }

    /// Writes the provider's row; a failing database is an `internal` error.
    async fn persist(self: &Arc<Self>, record: &ProviderRecord) -> Result<(), ErrorBody> {
        let row = ProviderRow {
            id: record.id.clone(),
            kind: record.kind.as_str().to_owned(),
            name: record.name.clone(),
            base_url: record.base_url.clone(),
            default_model: record.default_model.clone(),
            keychain_ref: keychain_ref(&record.id),
            created_at_ms: record.created_at_ms,
        };
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || storage.providers().upsert(row))
            .await
            .map_err(|e| internal("saving the provider", e))?
            .map_err(|e| internal("saving the provider", e))
    }

    // Talking to providers ----------------------------------------------------------------------

    /// The adapter of a provider, with its key read from the credential store. A provider that
    /// needs a key and has none fails here, before any request, with what to do about it.
    pub(crate) async fn resolve(
        self: &Arc<Self>,
        provider_id: &str,
    ) -> Result<(ProviderRecord, Arc<dyn Provider>), ErrorBody> {
        let record = self.record(provider_id)?;
        let this = self.clone();
        let account = record.id.clone();
        let key = tokio::task::spawn_blocking(move || this.store.get(&account))
            .await
            .map_err(|e| internal("reading the credential", e))?
            .map_err(|e| credential_error(&e))?;
        self.note_credential(&record.id, key.is_some());
        if key.is_none() && record.kind.requires_credential() {
            return Err(ErrorBody::new(
                ErrorCode::ProviderError,
                format!(
                    "provider `{id}` has no API key; store one with `hutan provider key {id}`",
                    id = record.id
                ),
            ));
        }
        let provider = connect(
            &ProviderConfig {
                kind: record.kind,
                base_url: Some(record.base_url.clone()),
                api_key: key,
            },
            &self.http,
        )
        .map_err(|e| gateway_error(&e))?;
        Ok((record, provider))
    }

    pub async fn models(self: &Arc<Self>, provider_id: &str) -> Result<ProviderModels, ErrorBody> {
        let (record, provider) = self.resolve(provider_id).await?;
        let mut models = provider
            .list_models()
            .await
            .map_err(|e| gateway_error(&e))?;
        for model in &mut models {
            model.is_default = record.default_model.as_deref() == Some(model.id.as_str());
        }
        Ok(ProviderModels {
            provider_id: record.id,
            models,
        })
    }

    /// Asks the well-known local ports what runs there. Once per request, never on a timer.
    pub async fn discover(self: &Arc<Self>) -> Result<ProviderDiscovery, ErrorBody> {
        let found = hutan_gateway::discovery::discover(&self.discovery).await;
        let known = self.providers.lock().clone();
        let found = found
            .into_iter()
            .map(|d| {
                let url = d.target.base_url.trim_end_matches('/');
                DiscoveredProvider {
                    kind: d.target.kind.as_str().to_owned(),
                    name: d.target.name.clone(),
                    base_url: d.target.base_url.clone(),
                    models: d.models.into_iter().map(|m| m.id).collect(),
                    provider_id: known
                        .iter()
                        .find(|p| p.base_url.trim_end_matches('/') == url)
                        .map(|p| p.id.clone()),
                }
            })
            .collect();
        Ok(ProviderDiscovery { found })
    }
}

fn checked_name(name: &str) -> Result<String, ErrorBody> {
    let name = name.trim();
    if name.is_empty() {
        return Err(bad_request("the provider name must not be empty"));
    }
    if name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(bad_request(format!(
            "the provider name must be text of at most {MAX_NAME_BYTES} bytes"
        )));
    }
    Ok(name.to_owned())
}

/// `None` and the empty string are both "no default model".
fn checked_model(model: Option<&str>) -> Result<Option<String>, ErrorBody> {
    let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) else {
        return Ok(None);
    };
    if model.len() > MAX_MODEL_BYTES || model.chars().any(char::is_control) {
        return Err(bad_request(format!(
            "a model name must be text of at most {MAX_MODEL_BYTES} bytes"
        )));
    }
    Ok(Some(model.to_owned()))
}

/// A lowercase ASCII slug of `name` (`My Router!` → `my-router`); the kind's name when nothing
/// usable is left.
fn slug(name: &str, kind: ProviderKind) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = &slug[..slug.len().min(40)];
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        kind.as_str().to_owned()
    } else {
        slug.to_owned()
    }
}

fn unique_id(providers: &[ProviderRecord], wanted: &str) -> String {
    if !providers.iter().any(|p| p.id == wanted) {
        return wanted.to_owned();
    }
    (2..)
        .map(|n| format!("{wanted}-{n}"))
        .find(|candidate| !providers.iter().any(|p| &p.id == candidate))
        .unwrap_or_else(|| wanted.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_short_ascii_words() {
        assert_eq!(slug("My Router!", ProviderKind::OpenAi), "my-router");
        assert_eq!(slug("  --  ", ProviderKind::Ollama), "ollama");
        assert_eq!(slug("ÄÖÜ", ProviderKind::Anthropic), "anthropic");
        assert_eq!(slug("OpenAI", ProviderKind::OpenAi), "openai");
        assert!(slug(&"a".repeat(100), ProviderKind::OpenAi).len() <= 40);
    }

    #[test]
    fn ids_get_a_numeric_suffix_when_taken() {
        let record = |id: &str| ProviderRecord {
            id: id.to_owned(),
            kind: ProviderKind::Ollama,
            name: id.to_owned(),
            base_url: "http://x".to_owned(),
            default_model: None,
            created_at_ms: 0,
        };
        let taken = [record("ollama"), record("ollama-2")];
        assert_eq!(unique_id(&taken, "ollama"), "ollama-3");
        assert_eq!(unique_id(&taken, "other"), "other");
    }

    #[tokio::test]
    async fn a_failing_database_fails_provider_changes_and_rolls_them_back() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("hutan.db");
        let storage = Storage::open(&database).unwrap();
        let service = ProviderService::new(
            storage.clone(),
            Arc::new(hutan_gateway::MemoryStore::new()),
            Http::new(hutan_gateway::HttpConfig::default()).unwrap(),
            Vec::new(),
            Vec::new(),
        );
        let add = |name: &str| ProviderAdd {
            kind: "ollama".to_owned(),
            name: name.to_owned(),
            base_url: None,
            default_model: None,
        };
        service.add(add("Local")).await.unwrap();

        // The table disappears under the running daemon, like a disk or schema fault.
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch("DROP TABLE providers")
            .unwrap();

        let error = service.add(add("Second")).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal, "{}", error.message);
        let error = service
            .update(ProviderUpdate {
                provider_id: "local".to_owned(),
                name: Some("Renamed".to_owned()),
                base_url: None,
                default_model: Some("llama3".to_owned()),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal, "{}", error.message);
        let error = service.remove("local").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal, "{}", error.message);

        // Nothing of the failed changes is visible: the list is what is stored.
        let list = service.list().await.unwrap().providers;
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].id, "local");
        assert_eq!(list[0].name, "Local");
        assert_eq!(list[0].default_model, None);
        storage.close();
    }
}
