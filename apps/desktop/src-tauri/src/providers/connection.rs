//! Named connection metadata store (charter §4.1). Metadata only — key
//! bytes live exclusively in VA3 custody (`crate::voice::keys`), referenced
//! by canonical base URL; nothing in this module can hold, serialize, or
//! log an `ApiKey`. Persisted as one JSON document in the `app_setting`
//! table per repository data practice; SQLite stores references, never
//! secrets (charter §4.2).

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::storage::settings::{get_setting, set_setting};
use crate::voice::keys::policy::{canonical_base_url, is_loopback_base_url};
use crate::voice::keys::UrlPolicyError;

use super::registry::{CredentialPolicy, EndpointPolicy, PhaseCapability, ProviderKind};

const STORE_KEY: &str = "provider_connections_v1";
const STORE_V2_KEY: &str = "provider_connections_v2";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionDocument {
    version: u32,
    connections: Vec<ConnectionRecord>,
}

/// Persisted connection metadata — exactly the charter §4.1 fields plus
/// lifecycle facts. `deny_unknown_fields` is the structural ban on parameter
/// creep (T2): a record carrying temperature/tokens/sampling fails to load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRecord {
    pub id: String,
    pub name: String,
    pub provider: ProviderKind,
    /// Canonical, present only for OpenAI-compatible connections; fixed
    /// providers own their URLs adapter-side (charter §6.3).
    pub base_url: Option<String>,
    /// Loopback-only credential-not-required (charter A1 safe default).
    pub credential_not_required: bool,
    pub default_model: String,
    pub enabled: bool,
    pub created_unix: i64,
    pub updated_unix: i64,
}

impl ConnectionRecord {
    /// An unsaved record for pre-creation model listing (owner feedback F1,
    /// 2026-08-29): the add-connection form lists models BEFORE a
    /// connection exists. The draft passes the exact validation the store
    /// runs — canonical URL, loopback-only keyless — so a draft can never
    /// egress where a saved connection could not.
    pub fn draft(
        provider_id: &str,
        base_url: Option<&str>,
        credential_not_required: bool,
    ) -> Result<ConnectionRecord, ConnectionError> {
        let provider = ProviderKind::from_id(provider_id)
            .ok_or_else(|| ConnectionError::UnknownProvider(provider_id.to_string()))?;
        validate(
            String::new(),
            "draft",
            provider,
            base_url,
            credential_not_required,
            "draft",
            0,
            0,
        )
    }

    /// The canonical base URL this connection's key custody and consent are
    /// scoped to (charter §4.2: keys isolated per canonical base URL; fixed
    /// providers use their adapter-owned host).
    pub fn key_base_url(&self) -> String {
        match self.provider.endpoint_policy() {
            EndpointPolicy::Fixed(url) => url.to_string(),
            EndpointPolicy::CustomRequired => self
                .base_url
                .clone()
                .expect("validation refuses custom connections without a base URL"),
        }
    }
}

/// The IPC-facing view: metadata plus key presence facts, never key bytes.
/// A serialization pin below asserts this exact field set — the shape IS
/// the custody boundary (charter §4.2: key material never crosses IPC).
/// camelCase on the wire like every other IPC view; the shared fixture in
/// `fixtures/providers/ipc_shapes/` is parsed by the frontend model tests,
/// so a drift on either side is a red test (checkpoint-3 blocker 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionView {
    pub id: String,
    pub name: String,
    pub provider_type: &'static str,
    pub hostname: Option<String>,
    /// The stored canonical base URL — present only for custom-endpoint
    /// connections (fixed providers own their URLs adapter-side). The
    /// update round-trip needs it; it is an endpoint fact, never a secret.
    pub base_url: Option<String>,
    pub default_model: String,
    pub capabilities: Vec<&'static str>,
    pub enabled: bool,
    pub key_required: bool,
    pub key_present: bool,
    pub key_last4: Option<String>,
}

impl ConnectionView {
    pub fn from_record(
        record: &ConnectionRecord,
        key_present: bool,
        key_last4: Option<String>,
    ) -> ConnectionView {
        ConnectionView {
            id: record.id.clone(),
            name: record.name.clone(),
            provider_type: record.provider.id(),
            hostname: match record.provider.endpoint_policy() {
                EndpointPolicy::CustomRequired => record
                    .base_url
                    .as_deref()
                    .and_then(|url| reqwest::Url::parse(url).ok())
                    .and_then(|url| url.host_str().map(str::to_string)),
                EndpointPolicy::Fixed(_) => None,
            },
            base_url: record.base_url.clone(),
            default_model: record.default_model.clone(),
            capabilities: record
                .provider
                .servable_phases()
                .iter()
                .map(|phase| match phase {
                    PhaseCapability::ScriptLlm => "script_llm",
                    PhaseCapability::TtsSynth => "tts",
                    PhaseCapability::VisionMllm => "vision_mllm",
                    #[cfg(feature = "book-agent")]
                    PhaseCapability::AgentLive => "agent_live",
                })
                .collect(),
            enabled: record.enabled,
            key_required: !record.credential_not_required,
            key_present,
            key_last4,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConnectionError {
    BlankName,
    BlankModel,
    UnknownProvider(String),
    BaseUrlForbidden,
    BaseUrlRequired,
    BaseUrl(UrlPolicyError),
    KeylessRequiresLoopback,
    NotFound(String),
    Storage(String),
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionError::BlankName => write!(f, "the connection needs a name"),
            ConnectionError::BlankModel => write!(f, "the connection needs a default model"),
            ConnectionError::UnknownProvider(id) => {
                write!(f, "unknown provider type {id:?}")
            }
            ConnectionError::BaseUrlForbidden => {
                write!(f, "this provider's endpoint is fixed and not editable")
            }
            ConnectionError::BaseUrlRequired => {
                write!(f, "an OpenAI-compatible connection needs a base URL")
            }
            ConnectionError::BaseUrl(err) => write!(f, "{err}"),
            ConnectionError::KeylessRequiresLoopback => write!(
                f,
                "no-key connections are allowed only for loopback localhost endpoints"
            ),
            ConnectionError::NotFound(id) => write!(f, "no connection {id:?}"),
            ConnectionError::Storage(err) => write!(f, "connection store: {err}"),
        }
    }
}

/// What deleting a connection would strand (charter §4.1: dependency-aware
/// delete). Jobs referencing the id are found by their persisted binding's
/// structural `"connection_id"` reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteImpact {
    pub referencing_jobs: Vec<String>,
    /// Cloud voice profiles whose structural `connection_id` names this
    /// connection (A15 Option 3): deleting it strands them, and the picker
    /// must be able to say so by name.
    pub referencing_voice_profiles: Vec<String>,
}

pub struct ConnectionStore;

fn store_mutex() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

impl ConnectionStore {
    pub async fn list(pool: &SqlitePool) -> Result<Vec<ConnectionRecord>, ConnectionError> {
        if let Some(raw) = get_setting(pool, STORE_V2_KEY)
            .await
            .map_err(|e| ConnectionError::Storage(e.to_string()))?
        {
            let document: ConnectionDocument =
                serde_json::from_str(&raw).map_err(|e| ConnectionError::Storage(e.to_string()))?;
            if document.version != 2 {
                return Err(ConnectionError::Storage(
                    "unsupported connection document version".to_string(),
                ));
            }
            return Ok(document.connections);
        }
        let raw = get_setting(pool, STORE_KEY)
            .await
            .map_err(|e| ConnectionError::Storage(e.to_string()))?;
        match raw {
            None => Ok(Vec::new()),
            Some(json) => {
                serde_json::from_str(&json).map_err(|e| ConnectionError::Storage(e.to_string()))
            }
        }
    }

    pub async fn get(pool: &SqlitePool, id: &str) -> Result<ConnectionRecord, ConnectionError> {
        Self::list(pool)
            .await?
            .into_iter()
            .find(|record| record.id == id)
            .ok_or_else(|| ConnectionError::NotFound(id.to_string()))
    }

    pub async fn create(
        pool: &SqlitePool,
        name: &str,
        provider_id: &str,
        base_url: Option<&str>,
        credential_not_required: bool,
        default_model: &str,
        now_unix: i64,
    ) -> Result<ConnectionRecord, ConnectionError> {
        let provider = ProviderKind::from_id(provider_id)
            .ok_or_else(|| ConnectionError::UnknownProvider(provider_id.to_string()))?;
        let record = validate(
            uuid::Uuid::now_v7().to_string(),
            name,
            provider,
            base_url,
            credential_not_required,
            default_model,
            now_unix,
            now_unix,
        )?;
        let _writer = store_mutex().lock().await;
        let mut all = Self::list(pool).await?;
        all.push(record.clone());
        Self::persist(pool, &all).await?;
        Ok(record)
    }

    /// Update mutable fields. Renaming preserves the id, so bindings and
    /// history survive (charter §4.1). Changing an OpenAI-compatible base
    /// URL rebinds key custody/consent scope by construction: custody is
    /// keyed by the canonical URL, so the old host's key never follows
    /// (charter §6.3).
    #[allow(clippy::too_many_arguments)]
    pub async fn update(
        pool: &SqlitePool,
        id: &str,
        name: &str,
        base_url: Option<&str>,
        credential_not_required: bool,
        default_model: &str,
        enabled: bool,
        now_unix: i64,
    ) -> Result<ConnectionRecord, ConnectionError> {
        let _writer = store_mutex().lock().await;
        let mut all = Self::list(pool).await?;
        let slot = all
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or_else(|| ConnectionError::NotFound(id.to_string()))?;
        let mut updated = validate(
            slot.id.clone(),
            name,
            slot.provider,
            base_url,
            credential_not_required,
            default_model,
            slot.created_unix,
            now_unix,
        )?;
        updated.enabled = enabled;
        let model_changed = slot.default_model != updated.default_model;
        *slot = updated.clone();
        if model_changed {
            let role_key = format!("provider_role_models.{id}");
            let mut roles: super::discovery::Recommendations = get_setting(pool, &role_key)
                .await
                .map_err(|error| ConnectionError::Storage(error.to_string()))?
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|error| ConnectionError::Storage(error.to_string()))?
                .unwrap_or_default();
            if matches!(
                updated.provider,
                ProviderKind::OpenaiTts | ProviderKind::CartesiaTts
            ) {
                roles.narration = Some(updated.default_model.clone());
            } else {
                roles.script = Some(updated.default_model.clone());
            }
            let mut transaction = pool
                .begin()
                .await
                .map_err(|error| ConnectionError::Storage(error.to_string()))?;
            for (key, value) in [
                (STORE_V2_KEY.to_string(), Self::document(&all)?),
                (
                    role_key,
                    serde_json::to_string(&roles)
                        .map_err(|error| ConnectionError::Storage(error.to_string()))?,
                ),
            ] {
                sqlx::query("INSERT INTO app_setting (key,value,updated_at) VALUES (?,?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at")
                    .bind(key).bind(value).bind(crate::storage::now_ms()).execute(&mut *transaction).await.map_err(|error| ConnectionError::Storage(error.to_string()))?;
            }
            transaction
                .commit()
                .await
                .map_err(|error| ConnectionError::Storage(error.to_string()))?;
        } else {
            Self::persist(pool, &all).await?;
        }
        Ok(updated)
    }

    pub async fn delete(pool: &SqlitePool, id: &str) -> Result<(), ConnectionError> {
        let _writer = store_mutex().lock().await;
        let mut all = Self::list(pool).await?;
        let before = all.len();
        all.retain(|record| record.id != id);
        if all.len() == before {
            return Err(ConnectionError::NotFound(id.to_string()));
        }
        Self::persist(pool, &all).await
    }

    /// Names what a delete would affect; the caller shows this before
    /// deleting. Historical jobs keep their display facts (their bindings
    /// snapshot provider/model strings), so the scan is about unfinished
    /// work, not history rewriting.
    pub async fn delete_impact(
        pool: &SqlitePool,
        id: &str,
    ) -> Result<DeleteImpact, ConnectionError> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT id, model_overrides_json FROM render_job \
             WHERE model_overrides_json IS NOT NULL \
             AND state NOT IN ('DONE', 'FAILED', 'CANCELLED')",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| ConnectionError::Storage(e.to_string()))?;
        let profiles: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT name, params_json FROM voice_profile WHERE params_json IS NOT NULL",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| ConnectionError::Storage(e.to_string()))?;
        Ok(DeleteImpact {
            referencing_jobs: rows
                .into_iter()
                .filter(|(_, overrides)| {
                    overrides
                        .as_deref()
                        .is_some_and(|json| overrides_reference_connection(json, id))
                })
                .map(|(id, _)| id)
                .collect(),
            referencing_voice_profiles: profiles
                .into_iter()
                .filter(|(_, params)| {
                    params
                        .as_deref()
                        .is_some_and(|json| overrides_reference_connection(json, id))
                })
                .map(|(name, _)| name)
                .collect(),
        })
    }

    pub(crate) async fn persist(
        pool: &SqlitePool,
        all: &[ConnectionRecord],
    ) -> Result<(), ConnectionError> {
        let json = Self::document(all)?;
        set_setting(pool, STORE_V2_KEY, &json)
            .await
            .map_err(|e| ConnectionError::Storage(e.to_string()))
    }

    pub(crate) fn document(all: &[ConnectionRecord]) -> Result<String, ConnectionError> {
        serde_json::to_string(&ConnectionDocument {
            version: 2,
            connections: all.to_vec(),
        })
        .map_err(|e| ConnectionError::Storage(e.to_string()))
    }

    pub(crate) fn store_key() -> &'static str {
        STORE_V2_KEY
    }
}

/// Structural reference check: any `connection_id` value equal to `id`
/// anywhere in the overrides document counts — JSON-parsed, never a
/// whitespace-sensitive substring match.
fn overrides_reference_connection(json: &str, id: &str) -> bool {
    fn walk(value: &serde_json::Value, id: &str) -> bool {
        match value {
            serde_json::Value::Object(map) => map.iter().any(|(key, child)| {
                (key == "connection_id" && child.as_str() == Some(id)) || walk(child, id)
            }),
            serde_json::Value::Array(items) => items.iter().any(|item| walk(item, id)),
            _ => false,
        }
    }
    serde_json::from_str::<serde_json::Value>(json)
        .map(|value| walk(&value, id))
        .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
fn validate(
    id: String,
    name: &str,
    provider: ProviderKind,
    base_url: Option<&str>,
    credential_not_required: bool,
    default_model: &str,
    created_unix: i64,
    updated_unix: i64,
) -> Result<ConnectionRecord, ConnectionError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ConnectionError::BlankName);
    }
    let default_model = default_model.trim();
    if default_model.is_empty() {
        return Err(ConnectionError::BlankModel);
    }
    let base_url = match (provider.endpoint_policy(), base_url) {
        (EndpointPolicy::Fixed(_), Some(_)) => return Err(ConnectionError::BaseUrlForbidden),
        (EndpointPolicy::Fixed(_), None) => None,
        (EndpointPolicy::CustomRequired, None) => return Err(ConnectionError::BaseUrlRequired),
        (EndpointPolicy::CustomRequired, Some(raw)) => {
            Some(canonical_base_url(raw).map_err(ConnectionError::BaseUrl)?)
        }
    };
    if credential_not_required {
        let loopback_ok = matches!(
            provider.credential_policy(),
            CredentialPolicy::KeyOrLoopbackKeyless
        ) && base_url.as_deref().is_some_and(is_loopback_base_url);
        if !loopback_ok {
            return Err(ConnectionError::KeylessRequiresLoopback);
        }
    }
    Ok(ConnectionRecord {
        id,
        name: name.to_string(),
        provider,
        base_url,
        credential_not_required,
        default_model: default_model.to_string(),
        enabled: true,
        created_unix,
        updated_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        (pool, dir)
    }

    #[tokio::test]
    async fn versioned_store_keeps_the_original_document_and_connection_identity() {
        let (pool, _dir) = pool().await;
        let record = validate(
            "legacy-id".to_string(),
            "My choice",
            ProviderKind::Gemini,
            None,
            false,
            "gemini-3.7-flash",
            1,
            2,
        )
        .unwrap();
        let original = serde_json::to_string(&vec![record.clone()]).unwrap();
        set_setting(&pool, STORE_KEY, &original).await.unwrap();
        assert_eq!(
            ConnectionStore::list(&pool).await.unwrap(),
            vec![record.clone()]
        );
        ConnectionStore::update(
            &pool,
            "legacy-id",
            "Renamed",
            None,
            false,
            "gemini-3.7-flash",
            false,
            3,
        )
        .await
        .unwrap();
        assert_eq!(
            get_setting(&pool, STORE_KEY).await.unwrap().as_deref(),
            Some(original.as_str())
        );
        let current = ConnectionStore::list(&pool).await.unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].id, record.id);
        assert_eq!(current[0].default_model, record.default_model);
        assert!(!current[0].enabled);
    }

    #[tokio::test]
    async fn create_list_update_rename_delete_round_trip() {
        let (pool, _dir) = pool().await;
        let created = ConnectionStore::create(
            &pool,
            " Work OpenRouter ",
            "openai_compatible",
            Some("https://OpenRouter.ai/api/v1/"),
            false,
            "some/model",
            100,
        )
        .await
        .unwrap();
        assert_eq!(created.name, "Work OpenRouter");
        assert_eq!(
            created.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(created.key_base_url(), "https://openrouter.ai/api/v1");

        let renamed = ConnectionStore::update(
            &pool,
            &created.id,
            "Personal OpenRouter",
            Some("https://openrouter.ai/api/v1"),
            false,
            "another/model",
            true,
            200,
        )
        .await
        .unwrap();
        assert_eq!(renamed.id, created.id, "rename preserves the stable id");
        assert_eq!(renamed.created_unix, 100);
        assert_eq!(renamed.updated_unix, 200);

        ConnectionStore::delete(&pool, &created.id).await.unwrap();
        assert!(ConnectionStore::list(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn validation_enforces_the_charter_field_law() {
        let (pool, _dir) = pool().await;
        assert_eq!(
            ConnectionStore::create(&pool, "  ", "gemini", None, false, "m", 0)
                .await
                .unwrap_err(),
            ConnectionError::BlankName
        );
        assert_eq!(
            ConnectionStore::create(&pool, "G", "gemini", None, false, " ", 0)
                .await
                .unwrap_err(),
            ConnectionError::BlankModel
        );
        assert_eq!(
            ConnectionStore::create(
                &pool,
                "G",
                "gemini",
                Some("https://elsewhere.example"),
                false,
                "m",
                0
            )
            .await
            .unwrap_err(),
            ConnectionError::BaseUrlForbidden,
            "fixed-provider URLs are adapter-owned"
        );
        assert_eq!(
            ConnectionStore::create(&pool, "C", "openai_compatible", None, false, "m", 0)
                .await
                .unwrap_err(),
            ConnectionError::BaseUrlRequired
        );
        assert!(
            matches!(
                ConnectionStore::create(
                    &pool,
                    "C",
                    "openai_compatible",
                    Some("http://192.168.1.20:8080/v1"),
                    false,
                    "m",
                    0
                )
                .await
                .unwrap_err(),
                ConnectionError::BaseUrl(_)
            ),
            "private-LAN HTTP stays refused (A2 safe default)"
        );
        assert_eq!(
            ConnectionStore::create(&pool, "X", "banana", None, false, "m", 0)
                .await
                .unwrap_err(),
            ConnectionError::UnknownProvider("banana".to_string())
        );
    }

    #[tokio::test]
    async fn keyless_is_loopback_only() {
        let (pool, _dir) = pool().await;
        let ollama = ConnectionStore::create(
            &pool,
            "Local Ollama",
            "openai_compatible",
            Some("http://localhost:11434/v1"),
            true,
            "llama3",
            0,
        )
        .await
        .unwrap();
        assert!(ollama.credential_not_required);

        assert_eq!(
            ConnectionStore::create(
                &pool,
                "Hosted keyless",
                "openai_compatible",
                Some("https://api.example.dev/v1"),
                true,
                "m",
                0
            )
            .await
            .unwrap_err(),
            ConnectionError::KeylessRequiresLoopback
        );
        assert_eq!(
            ConnectionStore::create(&pool, "G", "gemini", None, true, "m", 0)
                .await
                .unwrap_err(),
            ConnectionError::KeylessRequiresLoopback,
            "fixed hosted providers always require a key"
        );
    }

    #[test]
    fn drafts_pass_the_same_law_as_saved_connections() {
        let draft = ConnectionRecord::draft(
            "openai_compatible",
            Some("https://OpenRouter.ai/api/v1/"),
            false,
        )
        .unwrap();
        assert_eq!(draft.key_base_url(), "https://openrouter.ai/api/v1");
        assert!(
            ConnectionRecord::draft("openai_compatible", None, false).is_err(),
            "custom kind still requires a base URL"
        );
        assert_eq!(
            ConnectionRecord::draft(
                "openai_compatible",
                Some("https://api.example.dev/v1"),
                true
            )
            .unwrap_err(),
            ConnectionError::KeylessRequiresLoopback,
            "keyless stays loopback-only for drafts too"
        );
        assert!(ConnectionRecord::draft(
            "openai_compatible",
            Some("http://localhost:11434/v1"),
            true
        )
        .is_ok());
        assert!(matches!(
            ConnectionRecord::draft("banana", None, false).unwrap_err(),
            ConnectionError::UnknownProvider(_)
        ));
    }

    #[tokio::test]
    async fn zero_connections_is_the_empty_green_state() {
        let (pool, _dir) = pool().await;
        assert_eq!(ConnectionStore::list(&pool).await.unwrap(), Vec::new());
    }

    #[test]
    fn the_ipc_view_shape_is_pinned_and_secret_free() {
        let record = ConnectionRecord {
            id: "c-1".to_string(),
            name: "Work".to_string(),
            provider: ProviderKind::OpenaiCompatible,
            base_url: Some("https://api.example.dev/v1".to_string()),
            credential_not_required: false,
            default_model: "m".to_string(),
            enabled: true,
            created_unix: 0,
            updated_unix: 0,
        };
        let view = ConnectionView::from_record(&record, true, Some("ab12".to_string()));
        let json = serde_json::to_value(&view).unwrap();
        let mut fields: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            vec![
                "baseUrl",
                "capabilities",
                "defaultModel",
                "enabled",
                "hostname",
                "id",
                "keyLast4",
                "keyPresent",
                "keyRequired",
                "name",
                "providerType",
            ],
            "the IPC view is the custody boundary: metadata and last4 only, \
             and any new field is a deliberate re-pin"
        );
        assert_eq!(json["hostname"], "api.example.dev");
        assert_eq!(json["keyLast4"], "ab12");
    }

    /// Checkpoint-3 structural cure: the SAME fixture files the frontend
    /// model tests parse are asserted here byte-for-byte against the real
    /// serializers — a wire-contract drift on either side is a red test.
    #[test]
    fn connection_view_matches_the_shared_ipc_fixture() {
        let custom = ConnectionView::from_record(
            &ConnectionRecord {
                id: "c-custom".to_string(),
                name: "Work OpenRouter".to_string(),
                provider: ProviderKind::OpenaiCompatible,
                base_url: Some("https://openrouter.ai/api/v1".to_string()),
                credential_not_required: false,
                default_model: "some/model".to_string(),
                enabled: true,
                created_unix: 0,
                updated_unix: 0,
            },
            true,
            Some("ab12".to_string()),
        );
        let fixed = ConnectionView::from_record(
            &ConnectionRecord {
                id: "c-fixed".to_string(),
                name: "Gemini".to_string(),
                provider: ProviderKind::Gemini,
                base_url: None,
                credential_not_required: false,
                default_model: "gemini-3.7-flash".to_string(),
                enabled: false,
                created_unix: 0,
                updated_unix: 0,
            },
            false,
            None,
        );
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../fixtures/providers/ipc_shapes/connection_views.json"
        ))
        .unwrap();
        assert_eq!(
            serde_json::to_value([custom, fixed]).unwrap(),
            fixture,
            "connection_views.json is the two-consumer contract; regenerate \
             it deliberately when the view changes"
        );
    }

    #[test]
    fn delete_impact_matches_the_shared_ipc_fixture() {
        let impact = DeleteImpact {
            referencing_jobs: vec!["job-1".to_string(), "job-2".to_string()],
            referencing_voice_profiles: vec!["Aria (cloud)".to_string()],
        };
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../fixtures/providers/ipc_shapes/delete_impact.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(&impact).unwrap(), fixture);
    }

    #[test]
    fn delete_impact_reference_check_is_structural_not_substring() {
        let spaced = "{\"script\": {\"kind\": \"connection\", \"connection_id\" : \"c-1\", \"model\": \"m\"}}";
        assert!(overrides_reference_connection(spaced, "c-1"));
        assert!(!overrides_reference_connection(spaced, "c-2"));
        assert!(!overrides_reference_connection(
            "{\"note\": \"connection_id:c-1 as prose\"}",
            "c-1"
        ));
        assert!(!overrides_reference_connection("not json", "c-1"));
        assert!(overrides_reference_connection(
            "{\"phases\": [{\"connection_id\": \"c-1\"}]}",
            "c-1"
        ));
    }

    #[test]
    fn stored_records_refuse_parameter_creep() {
        let smuggled = "[{\"id\":\"c\",\"name\":\"n\",\"provider\":\"gemini\",\
                        \"base_url\":null,\"credential_not_required\":false,\
                        \"default_model\":\"m\",\"enabled\":true,\
                        \"created_unix\":0,\"updated_unix\":0,\
                        \"temperature\":0.7}]";
        assert!(serde_json::from_str::<Vec<ConnectionRecord>>(smuggled).is_err());
    }
}
