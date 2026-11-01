//! The Settings → Providers projection (S-5). One backend-derived view
//! feeds the top-level provider cards and each provider's management
//! sub-page: typed slot health per PF-PH1, pool labels, and the provider
//! card aggregate all computed HERE — React renders counts and labels, it
//! never infers health. Key bytes never appear in any type in this module
//! (masked last-four only, via the existing custody views).
//!
//! Families are the VA3 custody rows (the atomic pool is provider custody
//! identity + canonical base URL); registry kinds map onto them, so an
//! OpenAI TTS connection and an OpenAI-compatible Script connection meet
//! under one family exactly as their keys share one custody row.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use sqlx::SqlitePool;

use super::connection::{ConnectionStore, ConnectionView};
use super::pool_health::{
    dial_eligible, pool_label, provider_card_label, slot_health, PoolFacts, PoolLabel,
    ProviderCardLabel, SlotFacts, SlotHealth,
};
use super::registry::ProviderKind;
use crate::voice::keys::probe::Provider as VaultProvider;
use crate::voice::keys::KeyManager;

/// All custody families, in stable display order.
const FAMILIES: [VaultProvider; 5] = [
    VaultProvider::Gemini,
    VaultProvider::Anthropic,
    VaultProvider::OpenaiCompat,
    VaultProvider::Cartesia,
    VaultProvider::Deepgram,
];

fn family_display_name(family: VaultProvider) -> &'static str {
    match family {
        VaultProvider::Gemini => "Google Gemini",
        VaultProvider::Anthropic => "Anthropic",
        VaultProvider::OpenaiCompat => "OpenAI-compatible",
        VaultProvider::Cartesia => "Cartesia",
        VaultProvider::Deepgram => "Deepgram",
    }
}

/// Calm capability facts — what configured connections/keys can DO here,
/// never a catalog claim about the provider.
fn family_capabilities(family: VaultProvider) -> Vec<&'static str> {
    let mut labels: Vec<&'static str> = Vec::new();
    for kind in ProviderKind::ALL {
        if kind.vault_provider() == family {
            for phase in kind.servable_phases() {
                let label = match phase {
                    super::registry::PhaseCapability::ScriptLlm => "Scripting",
                    super::registry::PhaseCapability::TtsSynth => "Audio voices",
                    super::registry::PhaseCapability::VisionMllm => "Vision & charts",
                    // Not user-visible until B-4 mounts the entry; the label
                    // exists so the overview stays exhaustive-match honest.
                    #[cfg(feature = "book-agent")]
                    super::registry::PhaseCapability::AgentLive => "Book agent",
                };
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
        }
    }
    match family {
        VaultProvider::Gemini => labels.push("Voice Tutor"),
        VaultProvider::Deepgram => labels.push("Voice Tutor speech-to-text"),
        VaultProvider::Cartesia if !labels.contains(&"Voice Tutor speech") => {
            labels.push("Voice Tutor speech");
        }
        _ => {}
    }
    labels
}

/// Whether the Voice Tutor is a built-in consumer of this family's pool at
/// this canonical URL. The tutor needs no ConnectionRecord, so its pools
/// must not read as "Connection pool disabled" (sub-page rule 5: one page
/// serves batch AND Voice Tutor custody).
fn voice_tutor_uses(family: VaultProvider, base_url: &str) -> bool {
    matches!(
        family,
        VaultProvider::Gemini | VaultProvider::Deepgram | VaultProvider::Cartesia
    ) && family.default_base_url() == Some(base_url)
}

/// Typed per-state counts — the accessible truth the card exposes beside
/// its one aggregate (composition clarification 2).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SlotStateCounts {
    pub working: u32,
    pub checking: u32,
    pub unverified: u32,
    pub invalid: u32,
    pub rate_limited: u32,
    pub unreachable: u32,
    pub missing_bytes: u32,
    pub disabled: u32,
}

impl SlotStateCounts {
    fn add(&mut self, health: SlotHealth) {
        match health {
            SlotHealth::Working => self.working += 1,
            SlotHealth::Checking => self.checking += 1,
            SlotHealth::Unverified => self.unverified += 1,
            SlotHealth::Invalid => self.invalid += 1,
            SlotHealth::RateLimited => self.rate_limited += 1,
            SlotHealth::Unreachable => self.unreachable += 1,
            SlotHealth::MissingBytes => self.missing_bytes += 1,
            SlotHealth::Disabled => self.disabled += 1,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolSlotView {
    /// Opaque row identity for per-slot commands; never a key.
    pub account: String,
    pub slot: u32,
    pub label: Option<String>,
    pub last4: Option<String>,
    pub health: SlotHealth,
    pub dial_eligible: bool,
    /// Unix-ms deadline while a trustworthy rate-limit retry is pending.
    pub retry_after_unix_ms: Option<i64>,
    pub added_at: i64,
    pub last_used: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolView {
    pub base_url: String,
    pub label: PoolLabel,
    pub counts: SlotStateCounts,
    /// All stored slots, including Disabled.
    pub configured: u32,
    /// The operational health population (configured minus Disabled).
    pub operational: u32,
    pub voice_tutor_built_in: bool,
    pub slots: Vec<PoolSlotView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFamilyView {
    /// The VA3 custody id — the stable route key for the sub-page.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custody_id: Option<String>,
    pub display_name: String,
    pub capabilities: Vec<&'static str>,
    pub card: ProviderCardLabel,
    pub counts: SlotStateCounts,
    pub configured_keys: u32,
    pub enabled_keys: u32,
    pub connections: Vec<ConnectionView>,
    pub disabled_connections: u32,
    pub pools: Vec<PoolView>,
    /// Distinct default models of ENABLED connections — facts, not catalog.
    pub active_models: Vec<String>,
    pub vault_locked: bool,
    /// Provider retention/disclosure links, one per registry kind that
    /// publishes one (calm facts, adapter-owned URLs).
    pub retention_links: Vec<RetentionLink>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionLink {
    pub kind: &'static str,
    pub url: &'static str,
}

/// The typed explicit-pool-test progress event (S-5 blocker 6): ONE writer
/// (the `provider_pool_test` command) and ONE reader key on both sides —
/// the shape is pinned against the shared IPC fixture the frontend model
/// tests parse. `done` marks the terminal event (finished or cancelled).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolTestProgress {
    pub provider: String,
    pub base_url: String,
    pub completed: u32,
    pub total: u32,
    pub done: bool,
    pub cancelled: bool,
}

pub const POOL_TEST_PROGRESS_EVENT: &str = "provider-pool-test-progress";

/// Build the full projection. `Checking` never appears from here — an
/// explicit pool test reports typed progress through its own command and
/// the caller re-fetches when it settles.
pub async fn providers_overview(
    pool: &SqlitePool,
    keys: &Arc<KeyManager>,
) -> Result<Vec<ProviderFamilyView>, String> {
    let vault_locked = keys.storage_status().await.locked;
    let connections = ConnectionStore::list(pool)
        .await
        .map_err(|e| e.to_string())?;
    let entries = keys.list_entries(pool).await.map_err(|e| e.to_string())?;

    let mut families = Vec::new();
    for family in FAMILIES {
        let family_connections: Vec<_> = connections
            .iter()
            .filter(|record| record.provider.vault_provider() == family)
            .collect();

        // Pool map: canonical URL -> slots. Seeded from connections (a
        // keyless pool is still a pool) and from stored keys.
        let mut pools: BTreeMap<String, Vec<PoolSlotView>> = BTreeMap::new();
        for record in &family_connections {
            pools.entry(record.key_base_url()).or_default();
        }
        if let Some(default_url) = family.default_base_url() {
            if voice_tutor_uses(family, default_url) {
                pools.entry(default_url.to_string()).or_default();
            }
        }
        for entry in entries.iter().filter(|entry| entry.provider == family.id()) {
            // Rule-4 `Checking` is projected from the manager's in-flight
            // pool-test registry (S-5 blocker 6): the slot whose probe is
            // in flight right now reads Checking, and the pool label below
            // carries the typed (completed, total) progress.
            let test = keys.pool_test_status(family.id(), &entry.base_url);
            let facts = SlotFacts {
                state: entry.state,
                key_present: entry.key_present,
                disabled: keys.slot_disabled(pool, &entry.account).await,
                checking: test
                    .as_ref()
                    .is_some_and(|t| t.current_account.as_deref() == Some(&entry.account)),
                transport_failed: keys.transport_failed_for(&entry.account),
                retry_after_elapsed: keys.retry_after_elapsed(&entry.account),
            };
            let health = slot_health(&facts);
            pools
                .entry(entry.base_url.clone())
                .or_default()
                .push(PoolSlotView {
                    account: entry.account.clone(),
                    slot: entry.slot,
                    label: entry.label.clone(),
                    last4: entry.last4.clone(),
                    health,
                    dial_eligible: dial_eligible(health),
                    retry_after_unix_ms: keys.retry_after_until(&entry.account),
                    added_at: entry.added_at,
                    last_used: entry.last_used,
                });
        }

        let mut pool_views = Vec::new();
        let mut family_counts = SlotStateCounts::default();
        let mut configured_keys = 0u32;
        let mut enabled_keys = 0u32;
        for (base_url, slots) in pools {
            let referencing: Vec<_> = family_connections
                .iter()
                .filter(|record| {
                    super::directory::same_credential_group(
                        family.id(),
                        &record.key_base_url(),
                        &base_url,
                    )
                })
                .collect();
            let tutor = voice_tutor_uses(family, &base_url);
            let enabled_refs =
                referencing.iter().filter(|r| r.enabled).count() as u32 + u32::from(tutor);
            let all_loopback = !tutor
                && enabled_refs > 0
                && referencing
                    .iter()
                    .filter(|r| r.enabled)
                    .all(|r| r.credential_not_required);
            let healths: Vec<SlotHealth> = slots.iter().map(|s| s.health).collect();
            let earliest_retry = slots
                .iter()
                .filter(|s| s.health == SlotHealth::RateLimited)
                .filter_map(|s| s.retry_after_unix_ms)
                .min()
                .map(|ms| ms / 1000);
            let label = pool_label(&PoolFacts {
                enabled_connections: enabled_refs,
                all_enabled_loopback_keyless: all_loopback,
                vault_locked,
                explicit_test: keys
                    .pool_test_status(family.id(), &base_url)
                    .map(|t| (t.completed, t.total)),
                slots: healths.clone(),
                earliest_retry_unix: earliest_retry,
            });
            let mut counts = SlotStateCounts::default();
            for health in &healths {
                counts.add(*health);
            }
            let configured = slots.len() as u32;
            let operational = configured - counts.disabled;
            configured_keys += configured;
            enabled_keys += operational;
            family_counts = SlotStateCounts {
                working: family_counts.working + counts.working,
                checking: family_counts.checking + counts.checking,
                unverified: family_counts.unverified + counts.unverified,
                invalid: family_counts.invalid + counts.invalid,
                rate_limited: family_counts.rate_limited + counts.rate_limited,
                unreachable: family_counts.unreachable + counts.unreachable,
                missing_bytes: family_counts.missing_bytes + counts.missing_bytes,
                disabled: family_counts.disabled + counts.disabled,
            };
            pool_views.push(PoolView {
                base_url,
                label,
                counts,
                configured,
                operational,
                voice_tutor_built_in: tutor,
                slots,
            });
        }

        let configured = !family_connections.is_empty()
            || pool_views
                .iter()
                .any(|p| p.configured > 0 || p.voice_tutor_built_in);
        let labels: Vec<PoolLabel> = pool_views.iter().map(|p| p.label.clone()).collect();
        let card = provider_card_label(configured, &labels);

        let mut active_models: Vec<String> = family_connections
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.default_model.clone())
            .collect();
        active_models.sort();
        active_models.dedup();

        families.push(ProviderFamilyView {
            id: family.id().to_string(),
            custody_id: None,
            display_name: family_display_name(family).to_string(),
            capabilities: family_capabilities(family),
            card,
            counts: family_counts,
            configured_keys,
            enabled_keys,
            disabled_connections: family_connections.iter().filter(|r| !r.enabled).count() as u32,
            connections: {
                let mut views = Vec::new();
                for record in &family_connections {
                    let url = record.key_base_url();
                    let slot0 = entries.iter().find(|e| {
                        super::directory::same_credential_group(family.id(), &e.base_url, &url)
                            && e.provider == family.id()
                    });
                    views.push(ConnectionView::from_record(
                        record,
                        slot0.is_some_and(|e| e.key_present),
                        slot0.and_then(|e| e.last4.clone()),
                    ));
                }
                views
            },
            active_models,
            pools: pool_views,
            vault_locked,
            retention_links: ProviderKind::ALL
                .into_iter()
                .filter(|kind| kind.vault_provider() == family)
                .filter_map(|kind| {
                    kind.retention_url().map(|url| RetentionLink {
                        kind: kind.id(),
                        url,
                    })
                })
                .collect(),
        });
    }
    vendor_overview(pool, &families).await
}

async fn vendor_overview(
    pool: &SqlitePool,
    families: &[ProviderFamilyView],
) -> Result<Vec<ProviderFamilyView>, String> {
    let mut vendors = Vec::new();
    for definition in super::directory::definitions() {
        let Some(source) = families
            .iter()
            .find(|family| family.id == definition.custody_id)
        else {
            continue;
        };
        let mut connections: Vec<_> = source
            .connections
            .iter()
            .filter(|connection| {
                let url = connection
                    .base_url
                    .clone()
                    .or_else(|| {
                        ProviderKind::from_id(connection.provider_type).map(|kind| {
                            match kind.endpoint_policy() {
                                super::registry::EndpointPolicy::Fixed(url) => url.to_string(),
                                _ => String::new(),
                            }
                        })
                    })
                    .unwrap_or_default();
                super::directory::identity_for_url(&definition.custody_id, &url) == definition.id
            })
            .cloned()
            .collect();
        if let Some(catalog) = super::setup::cached_catalog(pool, &definition.id).await? {
            for connection in &mut connections {
                if catalog.models.iter().any(|model| {
                    model.id == connection.default_model
                        && model.vision == super::discovery::Support::Unsupported
                }) {
                    connection
                        .capabilities
                        .retain(|capability| *capability != "vision_mllm");
                }
            }
        }
        let pools: Vec<_> = source
            .pools
            .iter()
            .filter(|pool| {
                super::directory::identity_for_url(&definition.custody_id, &pool.base_url)
                    == definition.id
            })
            .cloned()
            .collect();
        let mut counts = SlotStateCounts::default();
        for pool in &pools {
            for slot in &pool.slots {
                counts.add(slot.health);
            }
        }
        let configured_keys = pools.iter().map(|pool| pool.configured).sum();
        let enabled_keys = pools.iter().map(|pool| pool.operational).sum();
        let labels = pools
            .iter()
            .map(|pool| pool.label.clone())
            .collect::<Vec<_>>();
        let card = provider_card_label(!connections.is_empty() || configured_keys > 0, &labels);
        let mut active_models = Vec::new();
        let mut capabilities = Vec::new();
        for connection in &connections {
            if connection.enabled {
                active_models.push(connection.default_model.clone());
                if let Some(model) = super::setup::role_models(pool, &connection.id)
                    .await?
                    .narration
                {
                    active_models.push(model);
                }
            }
            for capability in &connection.capabilities {
                if !capabilities.contains(capability) {
                    capabilities.push(*capability);
                }
            }
        }
        if capabilities.is_empty() {
            if !definition.preferred_script.is_empty() {
                capabilities.push("script_llm");
            }
            if !definition.preferred_narration.is_empty() {
                capabilities.push("tts");
            }
        }
        if definition.id == "gemini" {
            capabilities.push("Voice Tutor");
        }
        if definition.id == "cartesia" {
            capabilities.push("Voice Tutor speech");
        }
        if definition.id == "deepgram" {
            capabilities.push("Voice Tutor speech-to-text");
        }
        active_models.sort();
        active_models.dedup();
        vendors.push(ProviderFamilyView {
            id: definition.id.clone(),
            custody_id: Some(definition.custody_id.clone()),
            display_name: definition.name.clone(),
            capabilities,
            card,
            counts,
            configured_keys,
            enabled_keys,
            disabled_connections: connections
                .iter()
                .filter(|connection| !connection.enabled)
                .count() as u32,
            connections,
            pools,
            active_models,
            vault_locked: source.vault_locked,
            retention_links: definition
                .privacy_url
                .as_deref()
                .map(|url| RetentionLink {
                    kind: definition.id.as_str(),
                    url,
                })
                .into_iter()
                .collect(),
        });
    }
    Ok(vendors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compatible_services_keep_native_identity_connections_and_privacy_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        let mut connections = Vec::new();
        for (id, url) in [
            ("openai", "https://api.openai.com/v1"),
            ("groq", "https://api.groq.com/openai/v1"),
            ("custom", "https://api.openai.com/proxy"),
        ] {
            let mut record =
                crate::providers::ConnectionRecord::draft("openai_compatible", Some(url), false)
                    .unwrap();
            record.id = id.to_string();
            record.default_model = "publisher/ambiguous-model".to_string();
            connections.push(ConnectionView::from_record(&record, false, None));
        }
        let source = ProviderFamilyView {
            id: "openai_compat".into(),
            custody_id: None,
            display_name: "Transport".into(),
            capabilities: vec![],
            card: ProviderCardLabel::NotConfigured,
            counts: SlotStateCounts::default(),
            configured_keys: 0,
            enabled_keys: 0,
            connections,
            disabled_connections: 0,
            pools: vec![],
            active_models: vec![],
            vault_locked: false,
            retention_links: vec![],
        };
        let rows = vendor_overview(&pool, &[source]).await.unwrap();
        for id in ["openai", "groq", "custom"] {
            let row = rows.iter().find(|row| row.id == id).unwrap();
            assert_eq!(row.connections.len(), 1);
            assert_eq!(row.connections[0].id, id);
        }
        let groq = rows.iter().find(|row| row.id == "groq").unwrap();
        assert!(groq
            .retention_links
            .iter()
            .all(|link| link.url.contains("groq.com")));
        assert!(rows
            .iter()
            .find(|row| row.id == "custom")
            .unwrap()
            .retention_links
            .is_empty());
    }

    #[test]
    fn family_capabilities_are_calm_facts() {
        // No marketing vocabulary; the strings are phase/role facts.
        for family in FAMILIES {
            for label in family_capabilities(family) {
                for banned in ["best", "recommended", "premium", "upgrade", "pro "] {
                    assert!(
                        !label.to_lowercase().contains(banned),
                        "{label} contains {banned}"
                    );
                }
            }
        }
    }

    /// Checkpoint-3 structural cure: the SAME fixtures the frontend model
    /// tests parse are asserted here against the real serializers.
    #[test]
    fn the_family_projection_matches_the_shared_ipc_fixture() {
        let slot = PoolSlotView {
            account: "https://generativelanguage.googleapis.com".to_string(),
            slot: 0,
            label: Some("primary".to_string()),
            last4: Some("ab12".to_string()),
            health: SlotHealth::Working,
            dial_eligible: true,
            retry_after_unix_ms: None,
            added_at: 1000,
            last_used: Some(2000),
        };
        let pool_view = PoolView {
            base_url: "https://generativelanguage.googleapis.com".to_string(),
            label: PoolLabel::Checking {
                completed: 1,
                total: 3,
            },
            counts: SlotStateCounts {
                working: 1,
                ..SlotStateCounts::default()
            },
            configured: 1,
            operational: 1,
            voice_tutor_built_in: true,
            slots: vec![slot],
        };
        let family = ProviderFamilyView {
            id: "gemini".to_string(),
            custody_id: None,
            display_name: "Google Gemini".to_string(),
            capabilities: vec!["Scripting", "Voice Tutor"],
            card: ProviderCardLabel::Healthy,
            counts: SlotStateCounts {
                working: 1,
                ..SlotStateCounts::default()
            },
            configured_keys: 1,
            enabled_keys: 1,
            connections: vec![ConnectionView::from_record(
                &crate::providers::ConnectionRecord {
                    id: "c-1".to_string(),
                    name: "Gemini".to_string(),
                    provider: ProviderKind::Gemini,
                    base_url: None,
                    credential_not_required: false,
                    default_model: "gemini-3.7-flash".to_string(),
                    enabled: true,
                    created_unix: 0,
                    updated_unix: 0,
                },
                true,
                Some("ab12".to_string()),
            )],
            disabled_connections: 0,
            pools: vec![pool_view],
            active_models: vec!["gemini-3.7-flash".to_string()],
            vault_locked: false,
            retention_links: vec![RetentionLink {
                kind: "gemini",
                url: "https://ai.google.dev/gemini-api/terms",
            }],
        };
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../fixtures/providers/ipc_shapes/provider_family_view.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value([family]).unwrap(), fixture);
    }

    #[test]
    fn the_pool_test_progress_event_matches_the_shared_ipc_fixture() {
        let progress = PoolTestProgress {
            provider: "gemini".to_string(),
            base_url: "https://generativelanguage.googleapis.com".to_string(),
            completed: 1,
            total: 3,
            done: false,
            cancelled: false,
        };
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../fixtures/providers/ipc_shapes/pool_test_progress.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(&progress).unwrap(), fixture);
    }

    #[test]
    fn voice_tutor_is_a_built_in_consumer_only_at_its_default_urls() {
        assert!(voice_tutor_uses(
            VaultProvider::Gemini,
            VaultProvider::Gemini.default_base_url().unwrap()
        ));
        assert!(!voice_tutor_uses(
            VaultProvider::Gemini,
            "https://other.example"
        ));
        assert!(!voice_tutor_uses(
            VaultProvider::Anthropic,
            "https://api.anthropic.com"
        ));
    }
}
