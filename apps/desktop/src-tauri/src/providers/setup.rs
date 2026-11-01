use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::watch;

use super::directory::{definition, identity, ProviderDefinition};
use super::discovery::{self, Catalog, Recommendations};
use super::{ConnectionRecord, ConnectionStore, ProviderKind};
use crate::storage::settings::{get_setting, set_setting};
use crate::voice::keys::{ApiKey, KeyManager};

fn operations() -> &'static Mutex<HashMap<String, watch::Sender<bool>>> {
    static OPERATIONS: OnceLock<Mutex<HashMap<String, watch::Sender<bool>>>> = OnceLock::new();
    OPERATIONS.get_or_init(Default::default)
}

struct Operation(String);
impl Drop for Operation {
    fn drop(&mut self) {
        if let Ok(mut operations) = operations().lock() {
            operations.remove(&self.0);
        }
    }
}

fn cancelled_operations() -> &'static Mutex<std::collections::VecDeque<String>> {
    static CANCELLED: OnceLock<Mutex<std::collections::VecDeque<String>>> = OnceLock::new();
    CANCELLED.get_or_init(Default::default)
}

fn begin_operation(operation_id: &str) -> Result<(Operation, watch::Receiver<bool>), String> {
    uuid::Uuid::parse_str(operation_id).map_err(|_| "PROVIDER_INVALID_OPERATION")?;
    let mut registry = operations().lock().map_err(|_| "PROVIDER_BUSY")?;
    let mut cancelled = cancelled_operations().lock().map_err(|_| "PROVIDER_BUSY")?;
    if let Some(position) = cancelled.iter().position(|id| id == operation_id) {
        cancelled.remove(position);
        return Err("PROVIDER_CANCELLED".to_string());
    }
    if registry.contains_key(operation_id) {
        return Err("PROVIDER_BUSY".to_string());
    }
    let (sender, receiver) = watch::channel(false);
    registry.insert(operation_id.to_string(), sender);
    Ok((Operation(operation_id.to_string()), receiver))
}

pub fn cancel(operation_id: &str) {
    if uuid::Uuid::parse_str(operation_id).is_err() {
        return;
    }
    if let Ok(registry) = operations().lock() {
        if let Some(sender) = registry.get(operation_id) {
            sender.send_replace(true);
        } else if let Ok(mut cancelled) = cancelled_operations().lock() {
            if cancelled.len() >= 1024 {
                cancelled.pop_front();
            }
            cancelled.push_back(operation_id.to_string());
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupJournal {
    provider_id: String,
    finished: bool,
}

pub async fn cached_catalog(
    pool: &SqlitePool,
    provider_id: &str,
) -> Result<Option<Catalog>, String> {
    let provider = definition(provider_id)?;
    let raw = get_setting(pool, &format!("provider_catalog.{provider_id}"))
        .await
        .map_err(|_| "PROVIDER_SAVE_FAILED")?;
    match raw {
        Some(raw) => {
            let Ok(mut catalog) = serde_json::from_str::<Catalog>(&raw) else {
                return Ok(None);
            };
            if catalog.scope != cache_scope(pool, provider).await? {
                return Ok(None);
            }
            catalog.configured = current_configuration(pool, provider_id).await?;
            Ok(Some(catalog))
        }
        None => Ok(None),
    }
}

async fn cache_scope(pool: &SqlitePool, provider: &ProviderDefinition) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let configuration = KeyManager::configuration_fingerprint(
        pool,
        &provider.custody_id,
        provider.base_url.as_deref().unwrap_or(""),
    )
    .await
    .map_err(|_| "PROVIDER_SAVE_FAILED")?;
    let scope = serde_json::to_vec(&(
        &provider.id,
        &provider.base_url,
        &provider.api_version,
        configuration,
    ))
    .map_err(|_| "PROVIDER_INVALID_CONFIGURATION")?;
    Ok(hex::encode(Sha256::digest(scope)))
}

fn bind_scope(catalog: &mut Catalog, scope: String) {
    use sha2::{Digest, Sha256};
    if catalog.scope != scope {
        catalog.revision = hex::encode(Sha256::digest(format!("{}:{scope}", catalog.revision)));
        catalog.scope = scope;
    }
}

async fn current_configuration(
    pool: &SqlitePool,
    provider_id: &str,
) -> Result<Recommendations, String> {
    let records = ConnectionStore::list(pool)
        .await
        .map_err(|_| "PROVIDER_SAVE_FAILED")?;
    let profiles = crate::storage::voice::list_voice_profiles(pool)
        .await
        .map_err(|_| "PROVIDER_SAVE_FAILED")?;
    let mut selected = Recommendations::default();
    for record in records
        .iter()
        .filter(|record| record.enabled && identity(record) == provider_id)
    {
        let roles = role_models(pool, &record.id).await?;
        if selected.script.is_none()
            && record
                .provider
                .servable_phases()
                .contains(&super::registry::PhaseCapability::ScriptLlm)
        {
            selected.script = Some(record.default_model.clone());
        }
        if selected.narration.is_none()
            && record
                .provider
                .servable_phases()
                .contains(&super::registry::PhaseCapability::TtsSynth)
        {
            selected.narration = Some(narration_model(pool, record).await?);
            selected.voice = roles.voice.or_else(|| {
                profiles
                    .iter()
                    .find(|profile| {
                        super::cloud_profile_connection_id(profile.params_json.as_deref())
                            .as_deref()
                            == Some(&record.id)
                    })
                    .and_then(|profile| profile.preset_voice_id.clone())
            });
        }
    }
    Ok(selected)
}

pub async fn role_models(
    pool: &SqlitePool,
    connection_id: &str,
) -> Result<Recommendations, String> {
    let raw = get_setting(pool, &format!("provider_role_models.{connection_id}"))
        .await
        .map_err(|_| "PROVIDER_SAVE_FAILED")?;
    match raw {
        Some(raw) => {
            serde_json::from_str(&raw).map_err(|_| "PROVIDER_INVALID_CONFIGURATION".to_string())
        }
        None => Ok(Recommendations::default()),
    }
}

pub async fn narration_model(
    pool: &SqlitePool,
    record: &ConnectionRecord,
) -> Result<String, String> {
    let roles = role_models(pool, &record.id).await?;
    Ok(roles.narration.unwrap_or_else(|| {
        if record.provider == ProviderKind::Gemini {
            super::adapters::gemini_tts::resolve_model(&record.default_model).to_string()
        } else {
            record.default_model.clone()
        }
    }))
}

async fn write_setting(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    value: &str,
) -> Result<(), String> {
    sqlx::query("INSERT INTO app_setting (key,value,updated_at) VALUES (?,?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at")
        .bind(key).bind(value).bind(crate::storage::now_ms()).execute(&mut **transaction).await.map_err(|_| "PROVIDER_SAVE_FAILED")?;
    Ok(())
}

pub async fn provision(
    pool: &SqlitePool,
    provider: &ProviderDefinition,
    catalog: &Catalog,
    apply: bool,
    journal: Option<&str>,
    selected: Option<Recommendations>,
) -> Result<(), String> {
    provision_cancellable(pool, provider, catalog, apply, journal, selected, None).await
}

async fn provision_cancellable(
    pool: &SqlitePool,
    provider: &ProviderDefinition,
    catalog: &Catalog,
    apply: bool,
    journal: Option<&str>,
    selected: Option<Recommendations>,
    cancelled: Option<&watch::Receiver<bool>>,
) -> Result<(), String> {
    let _boundary = super::cast_plan::provider_mutation_mutex().lock().await;
    ensure_active(cancelled)?;
    let mut records = ConnectionStore::list(pool)
        .await
        .map_err(|_| "PROVIDER_SAVE_FAILED")?;
    let original_ids: std::collections::HashSet<_> =
        records.iter().map(|record| record.id.clone()).collect();
    let chosen = selected.unwrap_or_else(|| catalog.recommendations.clone());
    if (chosen.script.is_some() && provider.preferred_script.is_empty() && !provider.local)
        || (chosen.narration.is_some() && provider.preferred_narration.is_empty())
    {
        return Err("PROVIDER_INVALID_CONFIGURATION".to_string());
    }
    let mut updates: Vec<(String, Recommendations)> = Vec::new();
    let mut voices = Vec::new();
    for (role, model) in [
        ("script", chosen.script.as_ref()),
        ("narration", chosen.narration.as_ref()),
    ] {
        let Some(model) = model else {
            continue;
        };
        if model.trim().is_empty() {
            return Err("PROVIDER_INVALID_CONFIGURATION".to_string());
        }
        let kind = if role == "narration" && provider.id == "openai" {
            Some("openai_tts")
        } else {
            provider.provider_type.as_deref()
        };
        let Some(kind) = kind else {
            continue;
        };
        let existing = records
            .iter()
            .position(|record| identity(record) == provider.id && record.provider.id() == kind);
        let index = match existing {
            Some(index) => index,
            None => {
                let mut record = ConnectionRecord::draft(
                    kind,
                    if kind == "openai_compatible" {
                        provider.base_url.as_deref()
                    } else {
                        None
                    },
                    provider.local && !catalog.credential_required,
                )?;
                record.id = uuid::Uuid::now_v7().to_string();
                record.name = provider.name.clone();
                if role == "narration" && provider.id == "openai" {
                    record.name.push_str(" Audio");
                }
                record.default_model = model.clone();
                record.created_unix = crate::storage::now_ms() / 1000;
                record.updated_unix = record.created_unix;
                records.push(record);
                records.len() - 1
            }
        };
        let record = &mut records[index];
        if provider.local {
            record.credential_not_required = !catalog.credential_required;
        }
        let mut roles = updates
            .iter()
            .find(|(id, _)| id == &record.id)
            .map(|(_, roles)| roles.clone())
            .unwrap_or(role_models(pool, &record.id).await?);
        if role == "script" {
            if apply {
                record.default_model = model.clone();
            }
            roles.script = Some(record.default_model.clone());
        } else {
            if apply || roles.narration.is_none() {
                roles.narration = Some(if original_ids.contains(&record.id) && !apply {
                    narration_model(pool, record).await?
                } else {
                    model.clone()
                });
            }
            if apply && provider.id != "gemini" {
                record.default_model = model.clone();
            }
            if apply || roles.voice.is_none() {
                roles.voice = chosen.voice.clone();
                if provider.id == "gemini"
                    && !roles
                        .narration
                        .as_deref()
                        .unwrap_or("")
                        .starts_with("gemini-3.8-")
                {
                    roles.voice = roles.voice.map(|voice| {
                        super::adapters::gemini_tts::normalize_voice_name(&voice).to_string()
                    });
                }
            }
            if let Some(voice) = &roles.voice {
                voices.push((
                    format!("cloud-managed-{}-{}", provider.id, record.id),
                    format!("{} · {}", provider.name, voice),
                    voice.clone(),
                    record.id.clone(),
                ));
            }
        }
        if apply {
            record.updated_unix = crate::storage::now_ms() / 1000;
        }
        updates.retain(|(id, _)| id != &record.id);
        updates.push((record.id.clone(), roles));
    }
    let scope = cache_scope(pool, provider).await?;
    let mut transaction = pool.begin().await.map_err(|_| "PROVIDER_SAVE_FAILED")?;
    write_setting(
        &mut transaction,
        ConnectionStore::store_key(),
        &ConnectionStore::document(&records).map_err(|_| "PROVIDER_SAVE_FAILED")?,
    )
    .await?;
    for (id, roles) in &updates {
        write_setting(
            &mut transaction,
            &format!("provider_role_models.{id}"),
            &serde_json::to_string(roles).map_err(|_| "PROVIDER_SAVE_FAILED")?,
        )
        .await?;
    }
    for (id, name, voice, connection_id) in voices {
        let params = serde_json::json!({"connection_id":connection_id}).to_string();
        if apply {
            sqlx::query(
                "UPDATE voice_profile SET name=?, preset_voice_id=?, params_json=? WHERE id=?",
            )
            .bind(&name)
            .bind(&voice)
            .bind(&params)
            .bind(&id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| "PROVIDER_SAVE_FAILED")?;
        }
        sqlx::query("INSERT OR IGNORE INTO voice_profile (id,name,engine,kind,preset_voice_id,reference_wav_path,params_json,created_at) VALUES (?,?,'cloud','preset',?,NULL,?,?)")
            .bind(&id).bind(&name).bind(&voice).bind(&params).bind(crate::storage::now_ms()).execute(&mut *transaction).await.map_err(|_| "PROVIDER_SAVE_FAILED")?;
    }
    let mut configured_catalog = catalog.clone();
    bind_scope(&mut configured_catalog, scope);
    for (_, roles) in &updates {
        if roles.script.is_some() {
            configured_catalog.configured.script = roles.script.clone();
        }
        if roles.narration.is_some() {
            configured_catalog.configured.narration = roles.narration.clone();
            configured_catalog.configured.voice = roles.voice.clone();
        }
    }
    write_setting(
        &mut transaction,
        &format!("provider_catalog.{}", provider.id),
        &serde_json::to_string(&configured_catalog).map_err(|_| "PROVIDER_SAVE_FAILED")?,
    )
    .await?;
    if let Some(journal) = journal {
        write_setting(
            &mut transaction,
            journal,
            &serde_json::to_string(&SetupJournal {
                provider_id: provider.id.clone(),
                finished: true,
            })
            .map_err(|_| "PROVIDER_SAVE_FAILED")?,
        )
        .await?;
    }
    ensure_active(cancelled)?;
    transaction
        .commit()
        .await
        .map_err(|_| "PROVIDER_SAVE_FAILED")?;
    Ok(())
}

fn ensure_active(cancelled: Option<&watch::Receiver<bool>>) -> Result<(), String> {
    if cancelled.is_some_and(|receiver| *receiver.borrow()) {
        Err("PROVIDER_CANCELLED".to_string())
    } else {
        Ok(())
    }
}

pub async fn connect(
    pool: &SqlitePool,
    keys: &Arc<KeyManager>,
    provider_id: &str,
    key_raw: Option<String>,
    operation_id: &str,
    progress: impl Fn(&str),
) -> Result<Catalog, String> {
    uuid::Uuid::parse_str(operation_id).map_err(|_| "PROVIDER_INVALID_OPERATION")?;
    let provider = definition(provider_id)?;
    if provider.id == "custom" {
        return Err("PROVIDER_CUSTOM_SETUP_REQUIRED".to_string());
    }
    let journal = format!("provider_setup_operation.{operation_id}");
    if let Some(raw) = get_setting(pool, &journal)
        .await
        .map_err(|_| "PROVIDER_SAVE_FAILED")?
    {
        let saved: SetupJournal =
            serde_json::from_str(&raw).map_err(|_| "PROVIDER_INVALID_CONFIGURATION")?;
        if saved.provider_id != provider_id {
            return Err("PROVIDER_INVALID_OPERATION".to_string());
        }
        if saved.finished {
            return cached_catalog(pool, provider_id)
                .await?
                .ok_or_else(|| "PROVIDER_CATALOG_CHANGED".to_string());
        }
    }
    let (_operation, mut receiver) = begin_operation(operation_id)?;
    let supplied_key = key_raw.map(ApiKey::new).filter(|key| !key.is_empty());
    let reuse_key = supplied_key.is_none();
    let key = if reuse_key {
        keys.resolve_pool_key(
            pool,
            &provider.custody_id,
            provider
                .base_url
                .as_deref()
                .ok_or("PROVIDER_CUSTOM_SETUP_REQUIRED")?,
        )
        .await
        .ok()
        .map(|candidate| candidate.key)
    } else {
        supplied_key
    };
    if !provider.local && key.is_none() {
        return Err("PROVIDER_INVALID_KEY".to_string());
    }
    let initial_scope = cache_scope(pool, provider).await?;
    progress("discovering");
    let catalog = tokio::select! {
        result = discovery::discover(provider, key.as_ref()) => result?,
        _ = receiver.changed() => return Err("PROVIDER_CANCELLED".to_string()),
    };
    if provider.provider_type.is_some()
        && catalog.recommendations.script.is_none()
        && catalog.recommendations.narration.is_none()
    {
        return Err("PROVIDER_NO_SUPPORTED_MODELS".to_string());
    }
    if *receiver.borrow() {
        return Err("PROVIDER_CANCELLED".to_string());
    }
    if cache_scope(pool, provider).await? != initial_scope {
        return Err("PROVIDER_CATALOG_CHANGED".to_string());
    }
    progress("saving");
    set_setting(
        pool,
        &journal,
        &serde_json::to_string(&SetupJournal {
            provider_id: provider_id.to_string(),
            finished: false,
        })
        .map_err(|_| "PROVIDER_SAVE_FAILED")?,
    )
    .await
    .map_err(|_| "PROVIDER_SAVE_FAILED")?;
    if let Some(key) = key.filter(|_| !reuse_key) {
        let base = provider.base_url.as_deref();
        let key_url = if provider.custody_id == "openai_compat" {
            base
        } else {
            None
        };
        if let Some(base) = key_url {
            keys.record_consent(pool, base)
                .await
                .map_err(|_| "PROVIDER_SAVE_FAILED")?;
        }
        let entry = keys
            .add_setup_key(pool, &provider.custody_id, key_url, key, operation_id)
            .await
            .map_err(|_| "PROVIDER_KEY_STORAGE_FAILED")?;
        keys.record_probe_success(&entry.account);
    }
    provision_cancellable(
        pool,
        provider,
        &catalog,
        false,
        Some(&journal),
        None,
        Some(&receiver),
    )
    .await?;
    progress("ready");
    cached_catalog(pool, provider_id)
        .await?
        .ok_or_else(|| "PROVIDER_INVALID_CATALOG".to_string())
}

pub async fn refresh(
    pool: &SqlitePool,
    keys: &Arc<KeyManager>,
    provider_id: &str,
    operation_id: Option<&str>,
    continue_listing: bool,
) -> Result<Catalog, String> {
    let provider = definition(provider_id)?;
    let candidate = keys
        .resolve_pool_key(
            pool,
            &provider.custody_id,
            provider
                .base_url
                .as_deref()
                .ok_or("PROVIDER_CUSTOM_SETUP_REQUIRED")?,
        )
        .await;
    let key = match candidate {
        Ok(candidate) => Some(candidate.key),
        Err(_) if provider.local => None,
        Err(_) => return Err("PROVIDER_INVALID_KEY".to_string()),
    };
    let generated_id = uuid::Uuid::now_v7().to_string();
    let (_operation, mut receiver) = begin_operation(operation_id.unwrap_or(&generated_id))?;
    let previous = if continue_listing {
        cached_catalog(pool, provider_id).await?
    } else {
        None
    };
    let initial_scope = cache_scope(pool, provider).await?;
    let fetch = async {
        match previous {
            Some(previous) if previous.next_cursor.is_some() => {
                discovery::continue_discovery(provider, key.as_ref(), previous).await
            }
            _ => discovery::discover(provider, key.as_ref()).await,
        }
    };
    let mut catalog = tokio::select! {
        result = fetch => result?,
        _ = receiver.changed() => return Err("PROVIDER_CANCELLED".to_string()),
    };
    catalog.configured = current_configuration(pool, provider_id).await?;
    if cache_scope(pool, provider).await? != initial_scope {
        return Err("PROVIDER_CATALOG_CHANGED".to_string());
    }
    bind_scope(&mut catalog, initial_scope);
    ensure_active(Some(&receiver))?;
    set_setting(
        pool,
        &format!("provider_catalog.{provider_id}"),
        &serde_json::to_string(&catalog).map_err(|_| "PROVIDER_SAVE_FAILED")?,
    )
    .await
    .map_err(|_| "PROVIDER_SAVE_FAILED")?;
    Ok(catalog)
}

pub async fn apply(
    pool: &SqlitePool,
    provider_id: &str,
    revision: &str,
    selected: Option<Recommendations>,
) -> Result<(), String> {
    let provider = definition(provider_id)?;
    let catalog = cached_catalog(pool, provider_id)
        .await?
        .ok_or("PROVIDER_CATALOG_CHANGED")?;
    if catalog.revision != revision {
        return Err("PROVIDER_CATALOG_CHANGED".to_string());
    }
    if let Some(selected) = &selected {
        if let Some(voice) = &selected.voice {
            if !catalog.voices.iter().any(|entry| &entry.id == voice) {
                return Err("PROVIDER_INVALID_VOICE".to_string());
            }
        }
    }
    provision(pool, provider, &catalog, true, None, selected).await
}

pub async fn models_for_connection(
    pool: &SqlitePool,
    keys: &Arc<KeyManager>,
    connection: &ConnectionRecord,
) -> Result<(Vec<String>, Option<String>), String> {
    if !connection.enabled {
        return Err("PROVIDER_CONNECTION_DISABLED".to_string());
    }
    let identity = identity(connection);
    if identity == "custom" {
        let models = super::model_fetch::fetch_models_gated(pool, Some(keys), connection).await?;
        return Ok((models, None));
    }
    let catalog = refresh(pool, keys, identity, None, false).await?;
    let narration = matches!(
        connection.provider,
        ProviderKind::OpenaiTts | ProviderKind::CartesiaTts
    );
    let models = catalog.models.into_iter().filter(|model| if narration { model.narration } else { model.script } != discovery::Support::Unsupported).map(|model| model.id).collect();
    Ok((
        models,
        if narration {
            catalog.recommendations.narration
        } else {
            catalog.recommendations.script
        },
    ))
}

impl From<super::connection::ConnectionError> for String {
    fn from(error: super::connection::ConnectionError) -> Self {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(provider_id: &str) -> Catalog {
        let provider = definition(provider_id).unwrap();
        Catalog {
            provider_id: provider_id.to_string(),
            models: Vec::new(),
            voices: Vec::new(),
            configured: Recommendations::default(),
            recommendations: Recommendations {
                script: provider.preferred_script.first().cloned(),
                narration: provider.preferred_narration.first().cloned(),
                voice: provider.voices.first().cloned(),
            },
            revision: "revision-one".to_string(),
            scope: String::new(),
            fetched_unix: 1,
            complete: true,
            next_cursor: None,
            voices_complete: true,
            credential_required: true,
        }
    }

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        (dir, pool)
    }

    #[test]
    fn cancellation_before_admission_prevents_a_late_setup_from_starting() {
        let id = uuid::Uuid::now_v7().to_string();
        cancel(&id);
        assert!(matches!(begin_operation(&id), Err(error) if error == "PROVIDER_CANCELLED"));
        let id = uuid::Uuid::now_v7().to_string();
        let (_operation, receiver) = begin_operation(&id).unwrap();
        cancel(&id);
        assert!(*receiver.borrow());
    }

    #[tokio::test]
    async fn malformed_catalog_cache_does_not_block_recovery_or_change_connections() {
        let (_dir, pool) = pool().await;
        let record = ConnectionStore::create(
            &pool,
            "Existing",
            "gemini",
            None,
            false,
            "gemini-3.7-flash",
            1,
        )
        .await
        .unwrap();
        set_setting(&pool, "provider_catalog.gemini", "{interrupted")
            .await
            .unwrap();
        assert!(cached_catalog(&pool, "gemini").await.unwrap().is_none());
        assert_eq!(
            ConnectionStore::get(&pool, &record.id)
                .await
                .unwrap()
                .default_model,
            "gemini-3.7-flash"
        );
    }

    #[tokio::test]
    async fn cached_recommendations_are_scoped_to_the_vendor_credential_configuration() {
        let (_dir, pool) = pool().await;
        provision(
            &pool,
            definition("openai").unwrap(),
            &catalog("openai"),
            false,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(cached_catalog(&pool, "openai").await.unwrap().is_some());
        let metadata = r#"{"provider":"openai_compat","addedAt":1}"#;
        set_setting(&pool, "voice_key.https://api.groq.com/openai/v1", metadata)
            .await
            .unwrap();
        assert!(cached_catalog(&pool, "openai").await.unwrap().is_some());
        set_setting(&pool, "voice_key.https://api.openai.com", metadata)
            .await
            .unwrap();
        assert!(cached_catalog(&pool, "openai").await.unwrap().is_none());
        assert_eq!(
            apply(&pool, "openai", "old", None).await,
            Err("PROVIDER_CATALOG_CHANGED".to_string())
        );
    }

    #[tokio::test]
    async fn cancelled_provisioning_keeps_existing_settings_and_creates_no_voices() {
        let (_dir, pool) = pool().await;
        let (_sender, receiver) = watch::channel(true);
        assert_eq!(
            provision_cancellable(
                &pool,
                definition("openai").unwrap(),
                &catalog("openai"),
                false,
                None,
                None,
                Some(&receiver)
            )
            .await,
            Err("PROVIDER_CANCELLED".to_string())
        );
        assert!(ConnectionStore::list(&pool).await.unwrap().is_empty());
        assert!(crate::storage::voice::list_voice_profiles(&pool)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn setup_is_idempotent_and_records_separate_scripting_and_narration_models() {
        let (_dir, pool) = pool().await;
        let provider = definition("gemini").unwrap();
        let catalog = catalog("gemini");
        provision(&pool, provider, &catalog, false, None, None)
            .await
            .unwrap();
        provision(&pool, provider, &catalog, false, None, None)
            .await
            .unwrap();
        let records = ConnectionStore::list(&pool).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].default_model, "gemini-3.8-flash");
        assert_eq!(
            narration_model(&pool, &records[0]).await.unwrap(),
            "gemini-3.8-flash-lite-tts"
        );
        assert_eq!(
            crate::storage::voice::list_voice_profiles(&pool)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn reconnect_preserves_existing_choice_and_apply_changes_only_future_defaults() {
        let (_dir, pool) = pool().await;
        let record = ConnectionStore::create(
            &pool,
            "My Gemini",
            "gemini",
            None,
            false,
            "gemini-3.7-flash",
            1,
        )
        .await
        .unwrap();
        let catalog = catalog("gemini");
        provision(
            &pool,
            definition("gemini").unwrap(),
            &catalog,
            false,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            ConnectionStore::get(&pool, &record.id)
                .await
                .unwrap()
                .default_model,
            "gemini-3.7-flash"
        );
        assert_eq!(
            narration_model(&pool, &record).await.unwrap(),
            "gemini-3.1-flash-tts-preview"
        );
        assert!(apply(&pool, "gemini", "stale-revision", None)
            .await
            .is_err());
        let refreshed = cached_catalog(&pool, "gemini").await.unwrap().unwrap();
        apply(&pool, "gemini", &refreshed.revision, None)
            .await
            .unwrap();
        let updated = ConnectionStore::get(&pool, &record.id).await.unwrap();
        assert_eq!(updated.name, "My Gemini");
        assert_eq!(updated.default_model, "gemini-3.8-flash");
        assert_eq!(
            narration_model(&pool, &updated).await.unwrap(),
            "gemini-3.8-flash-lite-tts"
        );
        assert_eq!(record.default_model, "gemini-3.7-flash");
    }
}
