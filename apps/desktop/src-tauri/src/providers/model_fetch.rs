//! Explicit, bounded provider model listing (owner ruling 2026-08-28; T11:
//! user-triggered only, never a background dependency; T5: the result is a
//! suggestion list, manual model strings stay first-class).
//!
//! Endpoints (all already grounded in-repo or in module provenance):
//! - Anthropic: `GET /v1/models` — the same listing the S-0 VA3 probe pins
//!   (x-api-key + anthropic-version).
//! - Gemini: `GET /v1beta/models` with `x-goog-api-key`
//!   (https://ai.google.dev/api/models, checked 2026-08-29); entries named
//!   `models/<id>` and carrying `supportedGenerationMethods` (Pascal-case
//!   strings such as `generateContent`) — the scripting listing keeps
//!   entries supporting `generateContent` and, fail-open, entries where
//!   the field is absent (never over-filter; manual entry stands, T5).
//! - OpenAI-compatible: `GET {base}/models` (the stored base already ends
//!   in the provider's version segment where it has one); OpenAI TTS:
//!   `GET {base}/v1/models` on its fixed adapter-owned base. Bearer auth;
//!   loopback keyless sends no auth header, as everywhere else.
//! - Cartesia: the model set is a documented enum, not an API — the
//!   "fetch" returns the documented list.

use crate::providers::registry::{ProviderKind, ScriptError};
use crate::providers::ConnectionRecord;
use crate::voice::keys::ApiKey;

use super::adapters::{classify_status, http_client, malformed, read_bounded, transport_error};

/// A model list is names, not lectures: 1 MiB of JSON is already hundreds
/// of entries past any real listing.
const MAX_LIST_BYTES: usize = 1024 * 1024;
/// Curation bound (T11): a pathological listing is truncated, not rendered.
pub const MAX_MODELS: usize = 200;

async fn get_json(
    client: &reqwest::Client,
    url: String,
    headers: &[(&str, String)],
) -> Result<serde_json::Value, ScriptError> {
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let response = request.send().await.map_err(|err| transport_error(&err))?;
    let status = response.status().as_u16();
    if let Some(mut error) = classify_status(status, None) {
        let body = super::adapters::bounded_error_quote(response).await;
        if !body.is_empty() {
            error.message = format!("{} — {body}", error.message);
        }
        return Err(error);
    }
    let bytes = read_bounded(response, MAX_LIST_BYTES).await?;
    serde_json::from_slice(&bytes).map_err(|err| malformed(&err.to_string()))
}

/// The command core (S-5: the dev flag is retired). The release gate is
/// per-connection enablement — a disabled connection refuses BEFORE any
/// key custody lookup and before any network I/O — and custody itself is
/// PF-PH1 provider-plus-canonical-URL pool rotation, so this explicit
/// key-bearing egress can never dial with another provider's key.
pub async fn fetch_models_gated(
    pool: &sqlx::SqlitePool,
    keys: Option<&std::sync::Arc<crate::voice::keys::KeyManager>>,
    connection: &ConnectionRecord,
) -> Result<Vec<String>, String> {
    if !connection.enabled {
        return Err(format!(
            "connection '{}' is disabled; enable it in Settings → Providers to list models",
            connection.name
        ));
    }
    let (key, account) = if connection.credential_not_required {
        let keyless_lawful =
            matches!(
                connection.provider.credential_policy(),
                crate::providers::registry::CredentialPolicy::KeyOrLoopbackKeyless
            ) && crate::voice::keys::policy::is_loopback_base_url(&connection.key_base_url());
        if !keyless_lawful {
            return Err(format!(
                "connection '{}' specifies keyless access, but its provider/endpoint ({}) \
                 requires a key; keyless connections are allowed only for loopback endpoints",
                connection.name,
                connection.key_base_url()
            ));
        }
        (None, None)
    } else {
        let manager = keys.ok_or_else(|| "key custody unavailable".to_string())?;
        let candidate = manager
            .resolve_pool_key(
                pool,
                connection.provider.vault_provider().id(),
                &connection.key_base_url(),
            )
            .await
            .map_err(|e| e.to_string())?;
        (Some(candidate.key), Some(candidate.account))
    };
    let result = fetch_models(connection, key).await;
    // S-5 blocker 5: the settled outcome feeds slot state — success
    // advances rotation, auth/quota demotes the slot.
    if let (Some(account), Some(manager)) = (&account, keys) {
        match &result {
            Ok(_) => manager.record_pool_outcome(account, Ok(())),
            Err(error) => manager.record_pool_outcome(account, Err(error)),
        }
    }
    result.map_err(|e| e.message)
}

/// Gemini's scripting listing (F1 capability filter): entries whose
/// `supportedGenerationMethods` includes `generateContent`, plus entries
/// where the field is absent — a schema change must widen the list, never
/// silently empty it (the free-text override is the escape hatch either way).
fn gemini_script_models(payload: &serde_json::Value) -> Vec<String> {
    payload["models"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter(
                    |entry| match entry["supportedGenerationMethods"].as_array() {
                        Some(methods) => methods
                            .iter()
                            .any(|method| method.as_str() == Some("generateContent")),
                        None => true,
                    },
                )
                .filter_map(|entry| entry["name"].as_str())
                .map(|name| name.strip_prefix("models/").unwrap_or(name).to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub async fn fetch_models(
    connection: &ConnectionRecord,
    key: Option<ApiKey>,
) -> Result<Vec<String>, ScriptError> {
    let client = http_client()?;
    let base = connection.key_base_url();
    let mut models: Vec<String> = match connection.provider {
        ProviderKind::CartesiaTts => super::model_pick::cartesia_static_models(),
        ProviderKind::Gemini => {
            let key = key.ok_or_else(|| malformed("a Gemini connection requires a key"))?;
            let payload = get_json(
                &client,
                format!("{base}/v1beta/models"),
                &[("x-goog-api-key", key.expose().to_string())],
            )
            .await?;
            gemini_script_models(&payload)
        }
        ProviderKind::Anthropic => {
            let key = key.ok_or_else(|| malformed("an Anthropic connection requires a key"))?;
            let payload = get_json(
                &client,
                format!("{base}/v1/models"),
                &[
                    ("x-api-key", key.expose().to_string()),
                    ("anthropic-version", "2023-06-01".to_string()),
                ],
            )
            .await?;
            payload["data"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry["id"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        }
        ProviderKind::OpenaiCompatible | ProviderKind::OpenaiTts => {
            let url = if matches!(connection.provider, ProviderKind::OpenaiTts) {
                format!("{base}/v1/models")
            } else {
                format!("{base}/models")
            };
            let mut headers: Vec<(&str, String)> = Vec::new();
            if let Some(key) = &key {
                headers.push(("authorization", format!("Bearer {}", key.expose())));
            }
            let payload = get_json(&client, url, &headers).await?;
            let mut ids: Vec<String> = payload["data"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry["id"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if matches!(connection.provider, ProviderKind::OpenaiTts) {
                ids.retain(|id| id.contains("tts"));
            }
            ids
        }
    };
    models.sort();
    models.dedup();
    models.truncate(MAX_MODELS);
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn one_shot(body: String) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            let n = sock.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            crate::test_support::mock_http::write_response(
                &mut sock,
                "200 OK",
                "application/json",
                body.as_bytes(),
            )
            .await
            .unwrap();
            request
        });
        (base, server)
    }

    fn connection(provider: ProviderKind, base_url: Option<&str>) -> ConnectionRecord {
        ConnectionRecord {
            id: "c-1".to_string(),
            name: "Fixture".to_string(),
            provider,
            base_url: base_url.map(str::to_string),
            credential_not_required: base_url.is_some(),
            default_model: "m".to_string(),
            enabled: true,
            created_unix: 0,
            updated_unix: 0,
        }
    }

    #[tokio::test]
    async fn openai_compatible_lists_ids_from_the_models_endpoint() {
        let (base, server) = one_shot(
            r#"{"data":[{"id":"luna-5.6"},{"id":"luna-5.6-mini"},{"id":"luna-5.6"}]}"#.to_string(),
        )
        .await;
        let record = connection(ProviderKind::OpenaiCompatible, Some(&format!("{base}/v1")));
        let models = fetch_models(&record, None).await.unwrap();
        assert_eq!(models, vec!["luna-5.6", "luna-5.6-mini"], "sorted, deduped");
        let captured = server.await.unwrap();
        assert!(captured.contains("GET /v1/models"));
        assert!(
            !captured.to_lowercase().contains("authorization"),
            "loopback keyless sends no auth header"
        );
    }

    #[test]
    fn gemini_keeps_generate_content_models_and_fails_open_without_the_field() {
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"models":[
                {"name":"models/gemini-3.7-flash","supportedGenerationMethods":["generateContent","countTokens"]},
                {"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]},
                {"name":"models/gemini-mystery"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            gemini_script_models(&payload),
            vec!["gemini-3.7-flash", "gemini-mystery"],
            "generateContent kept, embed-only dropped, missing field fails open"
        );
    }

    #[tokio::test]
    async fn cartesia_returns_the_documented_enum_without_a_wire_call() {
        let record = connection(ProviderKind::CartesiaTts, None);
        let models = fetch_models(&record, None).await.unwrap();
        assert!(models.contains(&"sonic-latest".to_string()));
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    /// Hermes R2 F10 pin, S-5 form: the gate is now per-connection
    /// enablement — a DISABLED connection refuses before any custody or
    /// network access. The connection points at a dead endpoint and
    /// custody is absent; a custody touch would say "custody unavailable",
    /// a network touch would say transport. The refusal names enablement.
    #[tokio::test]
    async fn a_disabled_gate_refuses_before_custody_and_network() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        let record = ConnectionRecord {
            id: "c-1".to_string(),
            name: "Gemini".to_string(),
            provider: ProviderKind::Gemini,
            base_url: None,
            credential_not_required: false,
            default_model: "gemini-3.7-flash".to_string(),
            enabled: false,
            created_unix: 0,
            updated_unix: 0,
        };
        let error = fetch_models_gated(&pool, None, &record).await.unwrap_err();
        assert!(error.contains("disabled"), "{error}");
        assert!(
            !error.contains("custody unavailable") && !error.contains("transport"),
            "neither custody nor network was touched: {error}"
        );
    }
}
