//! The cast-submission writer boundary (Hermes S-4 pre-brief item 1): the
//! ONE place that turns picker selections into recorded bindings. It
//! validates connections, enforces consent for every non-loopback phase,
//! computes the cost snapshot, and emits the complete overrides additions —
//! all before the job row is inserted, so a job either carries its full
//! snapshot (binding + provider kind + canonical endpoint + adapter
//! revision + consent proof + cost) or does not exist. Stage code stays
//! read-only with respect to selection.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::orchestrator::state::Scope;
use crate::providers::binding::tts_binding_from_cloud_profile;
use crate::providers::consent::ConsentStore;
use crate::providers::cost::{self, CostSnapshot};
use crate::providers::registry::{PhaseCapability, ProviderKind};
use crate::providers::{ConnectionStore, PhaseBinding};
use crate::voice::keys::policy::is_loopback_base_url;

/// THE provider mutation boundary (Hermes R2 re-grade blocker 2): one lock
/// ordered around EVERY mutation that can change what a cast is authorized
/// against — consent grant/revoke, connection create/update/delete, book
/// deletion's consent cleanup — AND around the cast writer's own
/// plan-validate-insert span. A revocation issued while a cast is being
/// planned waits for the plan to finish (and the next plan sees it); a
/// plan started during a revocation sees the store after it. A lock that
/// only guarded plan+insert while mutators used their own locks
/// serialized nothing, which is exactly what the re-grade caught.
pub fn provider_mutation_mutex() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Backwards-compatible alias for the cast writer's span.
pub fn cast_writer_mutex() -> &'static tokio::sync::Mutex<()> {
    provider_mutation_mutex()
}

/// Grant a cast consent: subject validation (book exists, connection
/// enabled, phase servable) and the exact-displayed-tuple redemption run
/// INSIDE the mutation boundary, so a deletion in flight cannot leave an
/// orphan grant and a grant in flight cannot race a deletion (blocker 3).
pub async fn grant_cast_consent(
    pool: &SqlitePool,
    book_id: &str,
    connection_id: &str,
    phase: &str,
    shown_hosts: Vec<String>,
    shown_categories: Vec<String>,
    now_unix: i64,
) -> Result<crate::providers::consent::CastConsent, String> {
    let _boundary = provider_mutation_mutex().lock().await;
    crate::storage::book::get_book(pool, book_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("book '{book_id}' no longer exists"))?;
    let connection = ConnectionStore::get(pool, connection_id)
        .await
        .map_err(|e| e.to_string())?;
    if !connection.enabled {
        return Err(format!(
            "connection '{}' is disabled; nothing to consent to",
            connection.name
        ));
    }
    let phase_capability = match phase {
        "script" => PhaseCapability::ScriptLlm,
        "tts" => PhaseCapability::TtsSynth,
        "vision" => PhaseCapability::VisionMllm,
        other => return Err(format!("unknown consent phase {other:?}")),
    };
    if !connection
        .provider
        .servable_phases()
        .contains(&phase_capability)
    {
        return Err(format!(
            "provider '{}' cannot serve this phase; nothing to consent to",
            connection.provider.id()
        ));
    }
    let mut current_hosts = connection
        .provider
        .declared_hosts(connection.base_url.as_deref());
    current_hosts.sort();
    let mut shown_hosts = shown_hosts;
    shown_hosts.sort();
    shown_hosts.dedup();
    let mut current_categories = crate::providers::ProviderKind::data_categories(phase_capability);
    current_categories.sort();
    let mut shown_categories = shown_categories;
    shown_categories.sort();
    shown_categories.dedup();
    if shown_hosts != current_hosts || shown_categories != current_categories {
        return Err(
            "CONSENT_CHANGED: this connection's destination or data facts changed since the              sheet was shown; review the updated details"
                .to_string(),
        );
    }
    crate::providers::consent::ConsentStore::record(
        pool,
        book_id,
        &connection.id,
        connection.provider.id(),
        phase,
        current_hosts,
        current_categories,
        now_unix,
    )
    .await
    .map_err(|e| e.to_string())
}

pub async fn revoke_cast_consent(
    pool: &SqlitePool,
    consent_id: &str,
    now_unix: i64,
) -> Result<bool, String> {
    let _boundary = provider_mutation_mutex().lock().await;
    crate::providers::consent::ConsentStore::revoke(pool, consent_id, now_unix)
        .await
        .map_err(|e| e.to_string())
}

/// Book deletion's consent cleanup, inside the boundary: a grant racing a
/// deletion either lands before it (and is cleaned) or validates after it
/// (and refuses on the missing book) — never an orphan.
pub async fn delete_book_consents(pool: &SqlitePool, book_id: &str) -> Result<(), String> {
    let _boundary = provider_mutation_mutex().lock().await;
    crate::providers::consent::ConsentStore::delete_for_book(pool, book_id)
        .await
        .map_err(|e| e.to_string())
}

/// Connection deletion plus its consent cleanup as ONE boundary-held step.
pub async fn delete_connection_and_consents(
    pool: &SqlitePool,
    connection_id: &str,
) -> Result<(), String> {
    let _boundary = provider_mutation_mutex().lock().await;
    ConnectionStore::delete(pool, connection_id)
        .await
        .map_err(|e| e.to_string())?;
    crate::providers::consent::ConsentStore::delete_for_connection(pool, connection_id)
        .await
        .map_err(|e| e.to_string())
}

/// The writer-owned overrides keys: bindings, cost snapshots, consent
/// proof. NOTHING outside the writer boundary may supply them, on ANY
/// path — local, cloud, or mixed (Hermes R2 re-grade blocker 1): a forged
/// cost/consent snapshot on a local job is an audit-history lie even when
/// no cloud request follows.
pub const WRITER_OWNED_KEYS: [&str; 7] = [
    "script_binding",
    "tts_binding",
    "vision_binding",
    "cost_snapshots",
    "cast_consents",
    "recovery_log",
    "recovery_log_quarantined",
];

pub fn reject_writer_owned_keys(raw: Option<&str>) -> Result<(), String> {
    let Some(raw) = raw else {
        return Ok(());
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(());
    };
    for key in WRITER_OWNED_KEYS {
        if value.get(key).is_some() {
            return Err(format!(
                "'{key}' is written only by the cast form's own submission boundary; select                  providers there instead of supplying it"
            ));
        }
    }
    Ok(())
}

/// The AUTHORITATIVE cost basis (Hermes R2 fold-in 5): scoped source
/// characters derived from the book's stored canonical pages — never a
/// caller-supplied figure.
pub async fn scoped_source_chars(
    pool: &SqlitePool,
    book_id: &str,
    scope: &Scope,
) -> Result<u64, String> {
    let chapters = crate::storage::book::list_chapters(pool, book_id)
        .await
        .map_err(|e| e.to_string())?;
    // Selected pages are a SET, never a min..max envelope (Hermes R2
    // blocker 5): discontiguous chapter selections must not price the
    // unselected chapters between them.
    let mut selected_pages: std::collections::HashSet<i64> = std::collections::HashSet::new();
    match scope {
        Scope::Pages {
            page_start,
            page_end,
        } => {
            for page_no in *page_start..=*page_end {
                selected_pages.insert(page_no);
            }
        }
        Scope::Chapters { chapter_ids } => {
            for chapter in chapters
                .iter()
                .filter(|chapter| chapter_ids.contains(&chapter.id))
            {
                for page_no in chapter.page_start..=chapter.page_end {
                    selected_pages.insert(page_no);
                }
            }
        }
    }
    if selected_pages.is_empty() {
        return Ok(0);
    }
    let pages = crate::storage::book::list_pages(pool, book_id)
        .await
        .map_err(|e| e.to_string())?;
    let mut chars: u64 = 0;
    for page in pages {
        if !selected_pages.contains(&page.page_no) {
            continue;
        }
        let Some(raw) = page.text_json.as_deref() else {
            continue;
        };
        if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(raw) {
            if let Some(blocks) = envelope["blocks"].as_array() {
                for block in blocks {
                    if let Some(text) = block["text"].as_str() {
                        chars += text.chars().count() as u64;
                    }
                }
            }
        }
    }
    Ok(chars)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudScriptSelection {
    pub connection_id: String,
    /// Explicit model override; the connection's default when absent.
    pub model: Option<String>,
    /// Per-phase reasoning effort override (F6); the ruled LOW default
    /// when absent.
    #[serde(default)]
    pub effort: Option<crate::providers::registry::ReasoningEffort>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudTtsSelection {
    /// A15: the cloud voice profile IS the selection surface.
    pub voice_profile_id: String,
}

/// What the consent sheet must show for one not-yet-consented phase
/// (pre-brief item 6): connection, provider, exhaustive hosts, exact data
/// categories, retention link.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct ConsentRequirement {
    pub phase: String,
    pub connection_id: String,
    pub connection_name: String,
    pub provider: String,
    pub hosts: Vec<String>,
    pub data_categories: Vec<String>,
    pub retention_url: Option<String>,
}

#[derive(Debug)]
/// The consent question has three honest answers at plan time.
pub enum ConsentLookup {
    /// Loopback: consent does not apply (A18).
    Loopback,
    /// A valid grant exists — its identity is snapshotted into the job.
    Satisfied(crate::providers::consent::CastConsent),
    /// Start must wait for this grant.
    Missing(ConsentRequirement),
}

#[derive(Debug)]
pub struct CastPlan {
    /// The complete overrides document to persist with the job.
    pub overrides_json: String,
    /// Non-empty means Start must not proceed: these phases need consent.
    pub consent_missing: Vec<ConsentRequirement>,
    /// Truthful attribution (Hermes R2 cloud-profile ruling 4): when cloud
    /// TTS is selected, the job's voice_profile_id is the SELECTED cloud
    /// profile, so usage counts and history name the right voice.
    pub voice_profile_id: Option<String>,
}

async fn script_phase(
    pool: &SqlitePool,
    book_id: &str,
    selection: &CloudScriptSelection,
    source_chars: u64,
    now_unix: i64,
) -> Result<(PhaseBinding, CostSnapshot, ConsentLookup), String> {
    let connection = ConnectionStore::get(pool, &selection.connection_id)
        .await
        .map_err(|e| e.to_string())?;
    if !connection.enabled {
        return Err(format!(
            "connection '{}' is disabled; enable it or choose Local",
            connection.name
        ));
    }
    if !connection
        .provider
        .servable_phases()
        .contains(&PhaseCapability::ScriptLlm)
    {
        return Err(format!(
            "provider '{}' cannot serve Scripting",
            connection.provider.id()
        ));
    }
    let model = selection
        .model
        .clone()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or_else(|| connection.default_model.clone());
    let binding = PhaseBinding::Connection {
        connection_id: connection.id.clone(),
        model: model.clone(),
        provider: connection.provider.id().to_string(),
        endpoint: connection.key_base_url(),
        adapter_rev: connection.provider.adapter_revision().to_string(),
        effort: selection.effort.unwrap_or_default(),
    };
    let estimate = cost::estimate_script(
        cost::pricing_provider(&connection),
        &model,
        source_chars,
        &cost::iso_date_from_unix(now_unix),
    );
    let snapshot = CostSnapshot {
        phase: "script".to_string(),
        provider: connection.provider.id().to_string(),
        model,
        estimate,
        catalog_sha256: cost::catalog_sha256(),
    };
    let requirement = consent_requirement(
        pool,
        book_id,
        &connection,
        PhaseCapability::ScriptLlm,
        "script",
    )
    .await?;
    Ok((binding, snapshot, requirement))
}

async fn consent_requirement(
    pool: &SqlitePool,
    book_id: &str,
    connection: &crate::providers::ConnectionRecord,
    phase: PhaseCapability,
    phase_name: &str,
) -> Result<ConsentLookup, String> {
    // A18/T14: this-computer traffic gets neither consent theater nor cost
    // claims — loopback is exact, and private LAN is NOT loopback.
    if is_loopback_base_url(&connection.key_base_url()) {
        return Ok(ConsentLookup::Loopback);
    }
    let hosts = connection
        .provider
        .declared_hosts(connection.base_url.as_deref());
    let categories = ProviderKind::data_categories(phase);
    let existing = ConsentStore::find_valid(
        pool,
        book_id,
        &connection.id,
        connection.provider.id(),
        phase_name,
        hosts.clone(),
        categories.clone(),
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Some(existing) = existing {
        return Ok(ConsentLookup::Satisfied(existing));
    }
    Ok(ConsentLookup::Missing(ConsentRequirement {
        phase: phase_name.to_string(),
        connection_id: connection.id.clone(),
        connection_name: connection.name.clone(),
        provider: connection.provider.id().to_string(),
        hosts,
        data_categories: categories,
        retention_url: super::directory::definition(super::directory::identity(connection))
            .ok()
            .and_then(|definition| definition.privacy_url.clone()),
    }))
}

fn proof_of(consent: &crate::providers::consent::CastConsent) -> serde_json::Value {
    serde_json::json!({
        "consent_id": consent.id,
        "phase": consent.phase,
        "connection_id": consent.connection_id,
        "hosts": consent.hosts,
        "data_categories": consent.data_categories,
        "granted_unix": consent.granted_unix,
    })
}

/// Build the overrides document for a cast (the writer boundary). Existing
/// overrides keys (figure/llm/align choices) are preserved; binding, cost,
/// and consent keys are owned here.
#[allow(clippy::too_many_arguments)]
pub async fn plan_cast(
    pool: &SqlitePool,
    book_id: &str,
    existing_overrides: Option<&str>,
    script: Option<&CloudScriptSelection>,
    tts: Option<&CloudTtsSelection>,
    scope: Scope,
    mode: &str,
    now_unix: i64,
) -> Result<CastPlan, String> {
    let source_chars = scoped_source_chars(pool, book_id, &scope).await?;
    let mut overrides: serde_json::Value = existing_overrides
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    // Belt to the IPC-level rejection: writer-owned keys never survive from
    // caller input, whatever path reached here.
    if let Some(map) = overrides.as_object_mut() {
        for key in [
            "script_binding",
            "tts_binding",
            "vision_binding",
            "cost_snapshots",
            "cast_consents",
        ] {
            map.remove(key);
        }
    }
    let mut consent_missing = Vec::new();
    let mut consent_proof: Vec<serde_json::Value> = Vec::new();
    let mut snapshots: Vec<CostSnapshot> = Vec::new();

    let mut cloud_script_binding: Option<PhaseBinding> = None;
    if let Some(selection) = script {
        let (binding, snapshot, lookup) =
            script_phase(pool, book_id, selection, source_chars, now_unix).await?;
        overrides["script_binding"] = serde_json::to_value(&binding).map_err(|e| e.to_string())?;
        cloud_script_binding = Some(binding);
        snapshots.push(snapshot);
        match lookup {
            ConsentLookup::Missing(requirement) => consent_missing.push(requirement),
            ConsentLookup::Satisfied(consent) => consent_proof.push(proof_of(&consent)),
            ConsentLookup::Loopback => {}
        }
    }

    let mut cloud_voice_profile_id: Option<String> = None;
    if let Some(selection) = tts {
        cloud_voice_profile_id = Some(selection.voice_profile_id.clone());
        let profile = crate::storage::voice::get_voice_profile(pool, &selection.voice_profile_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "the selected voice profile no longer exists".to_string())?;
        let valid = crate::providers::validate_cloud_profile(
            &profile.engine,
            &profile.kind,
            profile.reference_wav_path.as_deref(),
            profile.preset_voice_id.as_deref(),
            profile.params_json.as_deref(),
        )
        .map_err(|reason| format!("{reason}; re-create it in Providers or choose a local voice"))?;
        let connection_id = valid.connection_id;
        let mut connection = ConnectionStore::get(pool, &connection_id)
            .await
            .map_err(|_| {
                "this cloud voice's connection was removed; re-create it in Providers or choose \
                 a local voice"
                    .to_string()
            })?;
        if !connection.enabled {
            return Err(format!(
                "connection '{}' is disabled; enable it or choose a local voice",
                connection.name
            ));
        }
        if !connection
            .provider
            .servable_phases()
            .contains(&PhaseCapability::TtsSynth)
        {
            return Err(format!(
                "provider '{}' cannot synthesize audio; this cloud voice cannot be cast",
                connection.provider.id()
            ));
        }
        connection.default_model = super::setup::narration_model(pool, &connection).await?;
        let binding =
            tts_binding_from_cloud_profile(profile.preset_voice_id.as_deref(), &connection)?;
        overrides["tts_binding"] = serde_json::to_value(&binding).map_err(|e| e.to_string())?;
        snapshots.push(CostSnapshot {
            phase: "tts".to_string(),
            provider: connection.provider.id().to_string(),
            model: connection.default_model.clone(),
            estimate: cost::estimate_tts(
                connection.provider.id(),
                &connection.default_model,
                source_chars,
                mode == "teacher",
                &cost::iso_date_from_unix(now_unix),
            ),
            catalog_sha256: cost::catalog_sha256(),
        });
        match consent_requirement(pool, book_id, &connection, PhaseCapability::TtsSynth, "tts")
            .await?
        {
            ConsentLookup::Missing(requirement) => consent_missing.push(requirement),
            ConsentLookup::Satisfied(consent) => consent_proof.push(proof_of(&consent)),
            ConsentLookup::Loopback => {}
        }
    }

    if let Some(PhaseBinding::Connection {
        connection_id,
        model,
        provider,
        endpoint,
        adapter_rev,
        ..
    }) = cloud_script_binding
    {
        if crate::orchestrator::vision_model::explain_figures(&overrides) {
            let connection = ConnectionStore::get(pool, &connection_id)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(catalog) =
                super::setup::cached_catalog(pool, super::directory::identity(&connection)).await?
            {
                if catalog.models.iter().any(|entry| {
                    entry.id == model && entry.vision == super::discovery::Support::Unsupported
                }) {
                    return Err("MODEL_NO_VISION: turn off figure explanation for this cast or choose a model that supports images".to_string());
                }
            }
            if connection
                .provider
                .servable_phases()
                .contains(&PhaseCapability::VisionMllm)
            {
                match consent_requirement(
                    pool,
                    book_id,
                    &connection,
                    PhaseCapability::VisionMllm,
                    "vision",
                )
                .await?
                {
                    ConsentLookup::Missing(requirement) => consent_missing.push(requirement),
                    ConsentLookup::Satisfied(consent) => consent_proof.push(proof_of(&consent)),
                    ConsentLookup::Loopback => {}
                }
                let vision = PhaseBinding::Connection {
                    connection_id,
                    model,
                    provider,
                    endpoint,
                    adapter_rev,
                    effort: crate::providers::registry::ReasoningEffort::Low,
                };
                overrides["vision_binding"] =
                    serde_json::to_value(&vision).map_err(|e| e.to_string())?;
            }
        }
    }

    if !snapshots.is_empty() {
        overrides["cost_snapshots"] =
            serde_json::to_value(&snapshots).map_err(|e| e.to_string())?;
    }
    if !consent_proof.is_empty() {
        // The job carries the consent it was AUTHORIZED under (fold-in 2):
        // history and recovery distinguish consent-at-Start from whatever
        // the store says later.
        overrides["cast_consents"] = serde_json::Value::Array(consent_proof);
    }
    Ok(CastPlan {
        overrides_json: overrides.to_string(),
        consent_missing,
        voice_profile_id: cloud_voice_profile_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        (dir, pool)
    }

    async fn seed_book(pool: &SqlitePool) -> String {
        let book = crate::storage::book::Book {
            id: crate::storage::new_id(),
            title: "Fixture".into(),
            author: None,
            source_path: "/books/f.pdf".into(),
            format: "pdf".into(),
            page_count: Some(2),
            language: Some("en".into()),
            is_scanned: false,
            ingest_path: Some("fast".into()),
            cover_path: None,
            content_hash: "hash".into(),
            blocks_version: Some("v1".into()),
            blocks_page_count: Some(2),
            layout_analysed: false,
            reflowed: false,
            extracting_since: None,
            created_at: crate::storage::now_ms(),
        };
        crate::storage::book::insert_book(pool, &book)
            .await
            .unwrap();
        for page_no in 1..=2 {
            crate::storage::book::insert_page(
                pool,
                &crate::storage::book::Page {
                    id: crate::storage::new_id(),
                    book_id: book.id.clone(),
                    page_no,
                    width_pt: Some(612.0),
                    height_pt: Some(792.0),
                    render_hash: None,
                    text_json: Some(
                        serde_json::json!({"quality": {}, "blocks": [
                            {"bbox": [0.0,0.0,1.0,1.0], "text": "x".repeat(1000),
                             "font_size": 11.0, "reading_order": 0, "column": 0}
                        ]})
                        .to_string(),
                    ),
                    needs_upgrade: false,
                },
            )
            .await
            .unwrap();
        }
        book.id
    }

    fn scope() -> Scope {
        Scope::Pages {
            page_start: 1,
            page_end: 2,
        }
    }

    async fn loopback_connection(pool: &SqlitePool) -> crate::providers::ConnectionRecord {
        ConnectionStore::create(
            pool,
            "Local Ollama",
            "openai_compatible",
            Some("http://127.0.0.1:11434/v1"),
            true,
            "llama3",
            1,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_writer_derives_its_basis_and_strips_caller_owned_keys() {
        let (_dir, pool) = pool().await;
        let connection = loopback_connection(&pool).await;
        // Caller-fed writer-owned keys are stripped (belt to the IPC
        // rejection); the basis is the 2000 stored characters, not any
        // caller figure.
        let plan = plan_cast(
            &pool,
            &seed_book(&pool).await,
            Some(r#"{"llm":"phi-4-mini","cost_snapshots":[{"fake":true}],"tts_binding":"x"}"#),
            Some(&CloudScriptSelection {
                connection_id: connection.id.clone(),
                model: None,
                effort: None,
            }),
            None,
            scope(),
            "teacher",
            1_787_875_200,
        )
        .await
        .unwrap();
        assert!(
            plan.consent_missing.is_empty(),
            "loopback needs no consent (A18)"
        );
        let overrides: serde_json::Value = serde_json::from_str(&plan.overrides_json).unwrap();
        assert_eq!(overrides["llm"], "phi-4-mini", "unowned keys survive");
        assert_eq!(
            overrides["script_binding"]["adapter_rev"],
            "openai_compat/1"
        );
        assert_eq!(
            overrides["cost_snapshots"][0]["estimate"]["state"], "unknown",
            "an unlisted model is honestly unknown, never $0 — and the fake caller \
             snapshot is gone"
        );
        assert!(overrides["cost_snapshots"][0]["estimate"]
            .get("basis")
            .is_none(),);
    }

    #[tokio::test]
    async fn a_cloud_script_model_also_explains_figures_under_its_own_snapshot() {
        let (_dir, pool) = pool().await;
        let connection = loopback_connection(&pool).await;
        let plan = plan_cast(
            &pool,
            &seed_book(&pool).await,
            None,
            Some(&CloudScriptSelection {
                connection_id: connection.id.clone(),
                model: Some("llava".into()),
                effort: None,
            }),
            None,
            scope(),
            "narrator",
            1_787_875_200,
        )
        .await
        .unwrap();
        let overrides: serde_json::Value = serde_json::from_str(&plan.overrides_json).unwrap();
        let recorded = &overrides["vision_binding"];
        assert_eq!(recorded["kind"], "connection");
        assert_eq!(recorded["connection_id"], connection.id.as_str());
        assert_eq!(
            recorded["model"], "llava",
            "the cast's script model, verbatim"
        );
        assert_eq!(recorded["provider"], "openai_compatible");
        assert_eq!(recorded["endpoint"], "http://127.0.0.1:11434/v1");
        assert_eq!(recorded["adapter_rev"], "openai_compat/1");
    }

    #[tokio::test]
    async fn explain_figures_off_records_no_vision_binding() {
        let (_dir, pool) = pool().await;
        let connection = loopback_connection(&pool).await;
        let plan = plan_cast(
            &pool,
            &seed_book(&pool).await,
            Some(r#"{"explain_figures":false}"#),
            Some(&CloudScriptSelection {
                connection_id: connection.id.clone(),
                model: None,
                effort: None,
            }),
            None,
            scope(),
            "narrator",
            1_787_875_200,
        )
        .await
        .unwrap();
        let overrides: serde_json::Value = serde_json::from_str(&plan.overrides_json).unwrap();
        assert!(overrides.get("vision_binding").is_none());
        assert_eq!(overrides["explain_figures"], false);
    }

    #[tokio::test]
    async fn a_caller_supplied_vision_binding_never_survives_the_writer() {
        let (_dir, pool) = pool().await;
        let plan = plan_cast(
            &pool,
            &seed_book(&pool).await,
            Some(r#"{"vision_binding":{"kind":"local","model_id":"x"}}"#),
            None,
            None,
            scope(),
            "narrator",
            1_787_875_200,
        )
        .await
        .unwrap();
        let overrides: serde_json::Value = serde_json::from_str(&plan.overrides_json).unwrap();
        assert!(overrides.get("vision_binding").is_none());
        assert!(reject_writer_owned_keys(Some(r#"{"vision_binding":null}"#)).is_err());
    }

    #[tokio::test]
    async fn a_remote_cloud_script_asks_vision_consent_for_figure_crops() {
        let (_dir, pool) = pool().await;
        let book_id = seed_book(&pool).await;
        let connection = ConnectionStore::create(
            &pool,
            "Gemini",
            "gemini",
            None,
            false,
            "gemini-3.7-flash",
            1,
        )
        .await
        .unwrap();
        let plan = plan_cast(
            &pool,
            &book_id,
            None,
            Some(&CloudScriptSelection {
                connection_id: connection.id.clone(),
                model: None,
                effort: None,
            }),
            None,
            scope(),
            "teacher",
            1_787_875_200,
        )
        .await
        .unwrap();
        let phases: Vec<&str> = plan
            .consent_missing
            .iter()
            .map(|r| r.phase.as_str())
            .collect();
        assert_eq!(phases, vec!["script", "vision"]);
        assert_eq!(
            plan.consent_missing[1].data_categories,
            vec!["figure_crops"]
        );
    }

    #[tokio::test]
    async fn a_remote_phase_without_consent_reports_and_a_grant_snapshots_proof() {
        let (_dir, pool) = pool().await;
        let book_id = seed_book(&pool).await;
        let connection = ConnectionStore::create(
            &pool,
            "Gemini",
            "gemini",
            None,
            false,
            "gemini-3.7-flash",
            1,
        )
        .await
        .unwrap();
        let selection = CloudScriptSelection {
            connection_id: connection.id.clone(),
            model: None,
            effort: None,
        };
        let no_figures = Some(r#"{"explain_figures":false}"#);
        let plan = plan_cast(
            &pool,
            &book_id,
            no_figures,
            Some(&selection),
            None,
            scope(),
            "teacher",
            1_787_875_200,
        )
        .await
        .unwrap();
        assert_eq!(plan.consent_missing.len(), 1);
        let requirement = &plan.consent_missing[0];
        assert_eq!(requirement.hosts, vec!["generativelanguage.googleapis.com"]);
        assert_eq!(requirement.data_categories, vec!["source_text"]);
        crate::providers::consent::ConsentStore::record(
            &pool,
            &book_id,
            &connection.id,
            "gemini",
            "script",
            requirement.hosts.clone(),
            requirement.data_categories.clone(),
            7,
        )
        .await
        .unwrap();
        let replanned = plan_cast(
            &pool,
            &book_id,
            no_figures,
            Some(&selection),
            None,
            scope(),
            "teacher",
            1_787_875_200,
        )
        .await
        .unwrap();
        assert!(replanned.consent_missing.is_empty());
        let overrides: serde_json::Value = serde_json::from_str(&replanned.overrides_json).unwrap();
        assert_eq!(overrides["cost_snapshots"][0]["estimate"]["state"], "known");
        // The consent PROOF is snapshotted into the job (fold-in 2).
        assert_eq!(overrides["cast_consents"][0]["phase"], "script");
        assert_eq!(overrides["cast_consents"][0]["granted_unix"], 7);
        assert_eq!(
            overrides["cast_consents"][0]["hosts"][0],
            "generativelanguage.googleapis.com"
        );
    }

    #[tokio::test]
    async fn a_stranded_cloud_voice_refuses_with_the_providers_remedy() {
        let (_dir, pool) = pool().await;
        let book_id = seed_book(&pool).await;
        let profile = crate::storage::voice::VoiceProfile {
            id: crate::storage::new_id(),
            name: "Cloud voice".to_string(),
            engine: crate::providers::CLOUD_VOICE_ENGINE.to_string(),
            kind: "preset".to_string(),
            preset_voice_id: Some("alloy".to_string()),
            reference_wav_path: None,
            params_json: Some(r#"{"connection_id":"gone"}"#.to_string()),
            created_at: 1,
        };
        crate::storage::voice::insert_voice_profile(&pool, &profile)
            .await
            .unwrap();
        let error = plan_cast(
            &pool,
            &book_id,
            None,
            None,
            Some(&CloudTtsSelection {
                voice_profile_id: profile.id,
            }),
            scope(),
            "narrator",
            1,
        )
        .await
        .unwrap_err();
        assert!(error.contains("Providers"), "{error}");
    }
}

#[cfg(test)]
mod writer_key_tests {
    use super::*;

    /// Hermes R2 blocker 1 pin: "the both-local path still ... persists
    /// caller-supplied cost_snapshots/cast_consents" — every writer-owned
    /// key refuses on every path, bindings AND audit facts.
    #[test]
    fn every_writer_owned_key_is_rejected_not_just_bindings() {
        for key in WRITER_OWNED_KEYS {
            let raw = format!("{{\"{key}\": {{\"forged\": true}}}}");
            let error = reject_writer_owned_keys(Some(&raw)).unwrap_err();
            assert!(error.contains(key), "{key} must refuse: {error}");
        }
        assert!(reject_writer_owned_keys(Some(r#"{"llm":"phi-4-mini"}"#)).is_ok());
        assert!(reject_writer_owned_keys(None).is_ok());
    }
}

#[cfg(test)]
mod mutation_boundary_tests {
    use super::*;

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        (dir, pool)
    }

    /// Hermes R2 blocker 2 pin: "Revocation can commit after consent
    /// validation and before job insertion" — a revoke issued while the
    /// cast writer holds the boundary WAITS for it; it cannot interleave
    /// between validation and insertion because they hold the same lock.
    #[tokio::test]
    async fn a_revocation_cannot_interleave_inside_the_cast_writer_span() {
        let (_dir, pool) = pool().await;
        let consent = crate::providers::consent::ConsentStore::record(
            &pool,
            "book-1",
            "c-1",
            "gemini",
            "script",
            vec!["generativelanguage.googleapis.com".to_string()],
            vec!["source_text".to_string()],
            1,
        )
        .await
        .unwrap();

        // The cast writer's span: validation has happened, insertion has not.
        let writer_guard = provider_mutation_mutex().lock().await;
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<bool>();
        let revoke_pool = pool.clone();
        let consent_id = consent.id.clone();
        let revoker = tokio::spawn(async move {
            let revoked = revoke_cast_consent(&revoke_pool, &consent_id, 2)
                .await
                .unwrap();
            let _ = done_tx.send(revoked);
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(
            done_rx.try_recv().is_err(),
            "the revoke must WAIT for the writer span, not interleave into it"
        );
        drop(writer_guard);
        tokio::time::timeout(std::time::Duration::from_secs(5), revoker)
            .await
            .expect("revoke completes once the span releases")
            .unwrap();
        assert!(done_rx.try_recv().unwrap_or(false), "the revoke did run");
        // And the NEXT plan sees the revocation: no valid consent remains.
        assert!(crate::providers::consent::ConsentStore::find_valid(
            &pool,
            "book-1",
            "c-1",
            "gemini",
            "script",
            vec!["generativelanguage.googleapis.com".to_string()],
            vec!["source_text".to_string()],
        )
        .await
        .unwrap()
        .is_none());
    }

    /// Hermes R2 blocker 3 pin: "deletion can complete cleanup and then
    /// grant can append an orphan" — grant validates its subjects INSIDE
    /// the same boundary deletion holds, so a grant after book deletion
    /// refuses on the missing book instead of orphaning a consent.
    #[tokio::test]
    async fn a_grant_racing_a_book_deletion_refuses_instead_of_orphaning() {
        let (_dir, pool) = pool().await;
        let connection = ConnectionStore::create(
            &pool,
            "Gemini",
            "gemini",
            None,
            false,
            "gemini-3.7-flash",
            1,
        )
        .await
        .unwrap();
        // Deletion holds the boundary; the racing grant queues behind it.
        let deletion_guard = provider_mutation_mutex().lock().await;
        let grant_pool = pool.clone();
        let connection_id = connection.id.clone();
        let grant = tokio::spawn(async move {
            grant_cast_consent(
                &grant_pool,
                "book-being-deleted",
                &connection_id,
                "script",
                vec!["generativelanguage.googleapis.com".to_string()],
                vec!["source_text".to_string()],
                3,
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(!grant.is_finished(), "the grant must wait for the deletion");
        // (The book never existed in this fixture — deletion "completed".)
        drop(deletion_guard);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), grant)
            .await
            .expect("grant settles")
            .unwrap();
        let error = result.expect_err("no orphan grant for a dead book");
        assert!(error.contains("no longer exists"), "{error}");
        assert!(crate::providers::consent::ConsentStore::list(&pool)
            .await
            .unwrap()
            .is_empty());
    }

    /// Connection deletion and its consent cleanup are one boundary-held
    /// step: nothing can observe the connection gone but its consents alive.
    #[tokio::test]
    async fn connection_deletion_and_consent_cleanup_are_one_step() {
        let (_dir, pool) = pool().await;
        let connection = ConnectionStore::create(
            &pool,
            "Gemini",
            "gemini",
            None,
            false,
            "gemini-3.7-flash",
            1,
        )
        .await
        .unwrap();
        crate::providers::consent::ConsentStore::record(
            &pool,
            "book-1",
            &connection.id,
            "gemini",
            "script",
            vec!["generativelanguage.googleapis.com".to_string()],
            vec!["source_text".to_string()],
            1,
        )
        .await
        .unwrap();
        delete_connection_and_consents(&pool, &connection.id)
            .await
            .unwrap();
        assert!(ConnectionStore::get(&pool, &connection.id).await.is_err());
        assert!(crate::providers::consent::ConsentStore::list(&pool)
            .await
            .unwrap()
            .is_empty());
    }
}

#[cfg(test)]
mod scope_basis_tests {
    use super::*;

    /// Hermes R2 blocker 5 pin: "Noncontiguous chapter selections are
    /// incorrectly priced as the entire min-to-max page envelope" — chapter
    /// 1 (pages 1-2) plus chapter 3 (pages 5-6) must price 4 pages, never 6.
    #[tokio::test]
    async fn discontiguous_chapters_price_the_selected_set_not_the_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        let book = crate::storage::book::Book {
            id: crate::storage::new_id(),
            title: "Fixture".into(),
            author: None,
            source_path: "/books/f.pdf".into(),
            format: "pdf".into(),
            page_count: Some(6),
            language: Some("en".into()),
            is_scanned: false,
            ingest_path: Some("fast".into()),
            cover_path: None,
            content_hash: "hash".into(),
            blocks_version: Some("v1".into()),
            blocks_page_count: Some(6),
            layout_analysed: false,
            reflowed: false,
            extracting_since: None,
            created_at: crate::storage::now_ms(),
        };
        crate::storage::book::insert_book(&pool, &book)
            .await
            .unwrap();
        for page_no in 1..=6 {
            crate::storage::book::insert_page(
                &pool,
                &crate::storage::book::Page {
                    id: crate::storage::new_id(),
                    book_id: book.id.clone(),
                    page_no,
                    width_pt: Some(612.0),
                    height_pt: Some(792.0),
                    render_hash: None,
                    text_json: Some(
                        serde_json::json!({"quality": {}, "blocks": [
                            {"bbox": [0.0,0.0,1.0,1.0], "text": "x".repeat(100),
                             "font_size": 11.0, "reading_order": 0, "column": 0}
                        ]})
                        .to_string(),
                    ),
                    needs_upgrade: false,
                },
            )
            .await
            .unwrap();
        }
        for (idx, (start, end)) in [(1i64, 2i64), (3, 4), (5, 6)].iter().enumerate() {
            crate::storage::book::insert_chapter(
                &pool,
                &crate::storage::book::Chapter {
                    id: format!("ch-{idx}"),
                    book_id: book.id.clone(),
                    idx: idx as i64,
                    title: format!("Chapter {idx}"),
                    page_start: *start,
                    page_end: *end,
                    is_pseudo: false,
                    word_count: Some(100),
                },
            )
            .await
            .unwrap();
        }
        let chars = scoped_source_chars(
            &pool,
            &book.id,
            &Scope::Chapters {
                chapter_ids: vec!["ch-0".to_string(), "ch-2".to_string()],
            },
        )
        .await
        .unwrap();
        assert_eq!(
            chars, 400,
            "pages 1,2,5,6 only — chapter 2's pages are not billed"
        );
    }
}

#[cfg(test)]
mod grant_redemption_tests {
    use super::*;

    /// Hermes matrix pin: a drifted displayed tuple refuses with
    /// CONSENT_CHANGED — the grant never authorizes what was not shown.
    #[tokio::test]
    async fn a_drifted_displayed_tuple_refuses_with_consent_changed() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        let book = crate::storage::book::Book {
            id: "book-1".to_string(),
            title: "Fixture".into(),
            author: None,
            source_path: "/b.pdf".into(),
            format: "pdf".into(),
            page_count: Some(1),
            language: None,
            is_scanned: false,
            ingest_path: None,
            cover_path: None,
            content_hash: "h".into(),
            blocks_version: None,
            blocks_page_count: None,
            layout_analysed: false,
            reflowed: false,
            extracting_since: None,
            created_at: 1,
        };
        crate::storage::book::insert_book(&pool, &book)
            .await
            .unwrap();
        let connection = ConnectionStore::create(&pool, "Gemini", "gemini", None, false, "m", 1)
            .await
            .unwrap();
        let error = grant_cast_consent(
            &pool,
            "book-1",
            &connection.id,
            "script",
            vec!["some-other-host.example".to_string()],
            vec!["source_text".to_string()],
            1,
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("CONSENT_CHANGED"), "{error}");
        assert!(crate::providers::consent::ConsentStore::list(&pool)
            .await
            .unwrap()
            .is_empty());
        // The exact displayed tuple grants.
        grant_cast_consent(
            &pool,
            "book-1",
            &connection.id,
            "script",
            vec!["generativelanguage.googleapis.com".to_string()],
            vec!["source_text".to_string()],
            1,
        )
        .await
        .unwrap();
    }
}
