//! Metadata contracts reviewed 2026-10-27; endpoint, auth/version and source URLs:
//! ../../fixtures/providers/directory.json. Conformance: discovery::tests.
//! OpenAI: https://developers.openai.com/api/reference/resources/models/methods/list
//! Gemini: https://ai.google.dev/api/models
//! Anthropic: https://platform.claude.com/docs/en/api/models/list
//! Cartesia: https://docs.cartesia.ai/api-reference/models/list
//! Other vendor sources are recorded per entry; catalog discovery never generates content.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::adapters::{http_client, read_bounded, MAX_SCRIPT_RESPONSE_BYTES};
use super::directory::{Authentication, ProviderDefinition};
use crate::voice::keys::ApiKey;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Supported,
    Unsupported,
    #[default]
    Unknown,
}

impl Support {
    fn from_bool(value: Option<bool>) -> Self {
        match value {
            Some(true) => Self::Supported,
            Some(false) => Self::Unsupported,
            None => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescriptor {
    pub id: String,
    pub name: String,
    pub script: Support,
    pub vision: Support,
    pub narration: Support,
    pub active: bool,
    pub preview: bool,
    pub context_window: Option<u64>,
    pub output_limit: Option<u64>,
    pub supported_parameters: Vec<String>,
    pub retirement: Option<String>,
    pub source: String,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub output_modalities: Vec<String>,
    #[serde(default)]
    pub lifecycle: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub released_at: Option<String>,
    #[serde(default)]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub protocol_capabilities: Option<Value>,
    #[serde(default)]
    pub effort_levels: Vec<String>,
    #[serde(default)]
    pub loaded: bool,
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceDescriptor {
    pub id: String,
    pub name: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recommendations {
    pub script: Option<String>,
    pub narration: Option<String>,
    pub voice: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub provider_id: String,
    pub models: Vec<ModelDescriptor>,
    pub voices: Vec<VoiceDescriptor>,
    pub recommendations: Recommendations,
    #[serde(default)]
    pub configured: Recommendations,
    pub revision: String,
    #[serde(default)]
    pub scope: String,
    pub fetched_unix: i64,
    pub complete: bool,
    pub next_cursor: Option<String>,
    pub voices_complete: bool,
    pub credential_required: bool,
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_models(
    provider: &ProviderDefinition,
    payload: &Value,
) -> Result<Vec<ModelDescriptor>, String> {
    let entries = payload
        .as_array()
        .or_else(|| payload["models"].as_array())
        .or_else(|| payload["data"].as_array())
        .or_else(|| payload["stt"].as_array())
        .ok_or_else(|| "PROVIDER_INVALID_CATALOG".to_string())?;
    entries
        .iter()
        .map(|entry| {
            let id = entry["id"]
                .as_str()
                .or_else(|| entry["key"].as_str())
                .or_else(|| entry["name"].as_str())
                .or_else(|| entry["canonical_name"].as_str())
                .ok_or_else(|| "PROVIDER_INVALID_CATALOG".to_string())?
                .trim_start_matches("models/")
                .to_string();
            let reviewed_script = provider.preferred_script.contains(&id);
            let reviewed_narration = provider.preferred_narration.contains(&id);
            let mut script = if reviewed_script {
                Support::Supported
            } else {
                Support::Unknown
            };
            let mut vision = if reviewed_script && provider.id != "groq" {
                Support::Supported
            } else {
                Support::Unknown
            };
            let mut narration = if reviewed_narration {
                Support::Supported
            } else {
                Support::Unknown
            };
            if provider.id == "groq" && reviewed_script {
                vision = Support::Unsupported;
            }
            if let Some(methods) = entry["supportedGenerationMethods"].as_array() {
                if !methods.iter().any(|method| method == "generateContent") || reviewed_narration {
                    script = Support::Unsupported;
                }
            }
            if let Some(value) = entry["capabilities"]["completion_chat"].as_bool() {
                script = Support::from_bool(Some(value));
            }
            if let Some(value) = entry["capabilities"]["vision"]
                .as_bool()
                .or_else(|| entry["capabilities"]["vision"]["supported"].as_bool())
                .or_else(|| entry["capabilities"]["image_input"]["supported"].as_bool())
            {
                vision = Support::from_bool(Some(value));
            }
            let modalities = entry
                .get("input_modalities")
                .or_else(|| entry["architecture"].get("input_modalities"));
            if let Some(modalities) = modalities.and_then(Value::as_array) {
                vision = Support::from_bool(Some(modalities.iter().any(|item| item == "image")));
                let outputs = entry
                    .get("output_modalities")
                    .or_else(|| entry["architecture"].get("output_modalities"));
                if let Some(outputs) = outputs.and_then(Value::as_array) {
                    script = Support::from_bool(Some(
                        outputs.iter().any(|item| item == "text")
                            && modalities.iter().any(|item| item == "text"),
                    ));
                }
            }
            if provider.id == "anthropic" {
                script = Support::Supported;
            }
            if provider.id == "lmstudio" {
                script = Support::from_bool(entry["type"].as_str().map(|kind| kind == "llm"));
            }
            if entry["capability"] == "tts" || provider.id == "cartesia" {
                narration = Support::Supported;
                script = Support::Unsupported;
                vision = Support::Unsupported;
            }
            if reviewed_narration {
                script = Support::Unsupported;
                vision = Support::Unsupported;
            }
            if provider.id == "deepgram" {
                script = Support::Unsupported;
                narration = Support::Unsupported;
                vision = Support::Unsupported;
            }
            let lifecycle = entry["lifecycle"].as_str().unwrap_or("active");
            Ok(ModelDescriptor {
                name: entry["display_name"]
                    .as_str()
                    .or_else(|| entry["displayName"].as_str())
                    .or_else(|| entry["name"].as_str())
                    .unwrap_or(&id)
                    .to_string(),
                id: id.clone(),
                script,
                vision,
                narration,
                active: entry["active"].as_bool().unwrap_or(true) && lifecycle == "active",
                preview: provider.preview_models.contains(&id)
                    || entry["preview"].as_bool().unwrap_or(false)
                    || id.contains("preview")
                    || id.starts_with("labs-"),
                context_window: entry["context_window"]
                    .as_u64()
                    .or_else(|| entry["context_length"].as_u64())
                    .or_else(|| entry["max_context_length"].as_u64())
                    .or_else(|| entry["inputTokenLimit"].as_u64())
                    .or_else(|| entry["max_input_tokens"].as_u64()),
                output_limit: entry["max_output_tokens"]
                    .as_u64()
                    .or_else(|| entry["outputTokenLimit"].as_u64())
                    .or_else(|| entry["max_tokens"].as_u64()),
                supported_parameters: strings(&entry["supported_parameters"]),
                retirement: entry["shutdown_date"]
                    .as_str()
                    .or_else(|| entry["retires_at"].as_str())
                    .map(str::to_string)
                    .or_else(|| provider.retirements.get(&id).cloned()),
                source: provider.documentation_url.clone(),
                input_modalities: modalities.map(strings).unwrap_or_default(),
                output_modalities: strings(
                    entry
                        .get("output_modalities")
                        .or_else(|| entry["architecture"].get("output_modalities"))
                        .unwrap_or(&Value::Null),
                ),
                lifecycle: lifecycle.to_string(),
                family: entry["line"].as_str().map(str::to_string),
                released_at: entry["created_at"]
                    .as_str()
                    .or_else(|| entry["modified_at"].as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        entry["created"]
                            .as_i64()
                            .map(|created| format!("{created:020}"))
                    }),
                protocols: if reviewed_narration && provider.id == "openai" {
                    vec!["realtime".to_string()]
                } else if let Some(capabilities) = entry["api_capabilities"].as_object() {
                    capabilities.keys().cloned().collect()
                } else {
                    provider.provider_type.iter().cloned().collect()
                },
                protocol_capabilities: entry.get("api_capabilities").cloned(),
                effort_levels: if let Some(effort) = entry["capabilities"]["effort"].as_object() {
                    effort
                        .iter()
                        .filter(|(_, value)| value["supported"] == true)
                        .map(|(level, _)| level.clone())
                        .collect()
                } else {
                    strings(&entry["effort"]["supported_levels"])
                },
                loaded: entry["loaded_instances"]
                    .as_array()
                    .is_some_and(|instances| !instances.is_empty()),
                size_bytes: entry["size_bytes"].as_u64(),
            })
        })
        .collect()
}

pub fn recommend(
    provider: &ProviderDefinition,
    models: &[ModelDescriptor],
    voices: &[VoiceDescriptor],
) -> Recommendations {
    let choose = |preferred: &[String], role: fn(&ModelDescriptor) -> Support| {
        preferred
            .iter()
            .find(|id| {
                models.iter().any(|model| {
                    &model.id == *id
                        && model.active
                        && !model.preview
                        && role(model) == Support::Supported
                })
            })
            .cloned()
            .or_else(|| {
                let mut compatible: Vec<_> = models
                    .iter()
                    .filter(|model| {
                        model.active && !model.preview && role(model) == Support::Supported
                    })
                    .collect();
                compatible.sort_by(|left, right| {
                    if provider.local {
                        (right.vision == Support::Supported)
                            .cmp(&(left.vision == Support::Supported))
                            .then(right.loaded.cmp(&left.loaded))
                            .then(
                                left.size_bytes
                                    .unwrap_or(u64::MAX)
                                    .cmp(&right.size_bytes.unwrap_or(u64::MAX)),
                            )
                            .then(left.id.cmp(&right.id))
                    } else {
                        let price = |model: &ModelDescriptor| {
                            super::cost::reviewed_input_price(&provider.id, &model.id)
                                .unwrap_or(f64::MAX)
                        };
                        price(left)
                            .total_cmp(&price(right))
                            .then(right.released_at.cmp(&left.released_at))
                            .then(left.id.cmp(&right.id))
                    }
                });
                compatible.first().map(|model| model.id.clone())
            })
    };
    let mut sonnets: Vec<_> = models
        .iter()
        .filter(|model| {
            model.family.as_deref() == Some("sonnet")
                && model.active
                && !model.preview
                && model.script == Support::Supported
        })
        .collect();
    sonnets.sort_by(|left, right| {
        right
            .released_at
            .cmp(&left.released_at)
            .then(left.id.cmp(&right.id))
    });
    let script = if provider.id == "anthropic" {
        sonnets
            .first()
            .map(|model| model.id.clone())
            .or_else(|| choose(&provider.preferred_script, |model| model.script))
    } else {
        choose(&provider.preferred_script, |model| model.script)
    };
    Recommendations {
        script,
        narration: choose(&provider.preferred_narration, |model| model.narration),
        voice: voices.first().map(|voice| voice.id.clone()),
    }
}

async fn get_json(
    provider: &ProviderDefinition,
    url: &str,
    key: Option<&ApiKey>,
) -> Result<Value, String> {
    metadata_json(provider, url, key, None).await
}

async fn metadata_json(
    provider: &ProviderDefinition,
    url: &str,
    key: Option<&ApiKey>,
    body: Option<&Value>,
) -> Result<Value, String> {
    let client = http_client().map_err(|_| "PROVIDER_UNREACHABLE".to_string())?;
    let mut request = match body {
        Some(body) => client.post(url).json(body),
        None => client.get(url),
    };
    if let Some(key) = key {
        request = match provider.authentication {
            Authentication::GoogleHeader => request.header("x-goog-api-key", key.expose()),
            Authentication::DeepgramToken => {
                request.header("authorization", format!("Token {}", key.expose()))
            }
            Authentication::Bearer | Authentication::OptionalBearer => {
                request.header("authorization", format!("Bearer {}", key.expose()))
            }
        };
    }
    if let Some(version) = &provider.api_version {
        request = request.header(
            if provider.id == "cartesia" {
                "Cartesia-Version"
            } else {
                "anthropic-version"
            },
            version,
        );
    }
    let response = request
        .send()
        .await
        .map_err(|_| "PROVIDER_UNREACHABLE".to_string())?;
    let code = match response.status().as_u16() {
        200..=299 => None,
        401 => Some("PROVIDER_INVALID_KEY"),
        403 => Some("PROVIDER_PERMISSION_REQUIRED"),
        402 | 429 => Some("PROVIDER_RATE_LIMITED"),
        404 => Some("PROVIDER_DISCOVERY_UNAVAILABLE"),
        _ => Some("PROVIDER_UNREACHABLE"),
    };
    if let Some(code) = code {
        return Err(code.to_string());
    }
    let bytes = read_bounded(response, MAX_SCRIPT_RESPONSE_BYTES)
        .await
        .map_err(|_| "PROVIDER_INVALID_CATALOG".to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "PROVIDER_INVALID_CATALOG".to_string())
}

fn next_page(provider: &ProviderDefinition, payload: &Value, url: &str) -> Option<String> {
    let mut next = reqwest::Url::parse(url).ok()?;
    if let Some(token) = payload["nextPageToken"].as_str() {
        next.query_pairs_mut()
            .clear()
            .append_pair("pageToken", token);
    } else if payload["has_more"].as_bool() == Some(true) {
        let cursor = payload["last_id"]
            .as_str()
            .or_else(|| payload["data"].as_array()?.last()?["id"].as_str())?;
        next.query_pairs_mut().clear().append_pair(
            if provider.id == "anthropic" {
                "after_id"
            } else {
                "starting_after"
            },
            cursor,
        );
    } else {
        return None;
    }
    Some(next.to_string())
}

pub async fn discover(
    provider: &ProviderDefinition,
    key: Option<&ApiKey>,
) -> Result<Catalog, String> {
    discover_pages(provider, key, None).await
}

pub async fn continue_discovery(
    provider: &ProviderDefinition,
    key: Option<&ApiKey>,
    previous: Catalog,
) -> Result<Catalog, String> {
    discover_pages(provider, key, Some(previous)).await
}

fn checked_cursor(initial: &str, cursor: &str) -> Result<String, String> {
    let initial = reqwest::Url::parse(initial).map_err(|_| "PROVIDER_INVALID_CATALOG")?;
    let cursor = reqwest::Url::parse(cursor).map_err(|_| "PROVIDER_INVALID_CATALOG")?;
    if initial.origin() != cursor.origin()
        || initial.path() != cursor.path()
        || !cursor.username().is_empty()
        || cursor.password().is_some()
        || cursor.fragment().is_some()
        || cursor.as_str().len() > 8192
    {
        return Err("PROVIDER_INVALID_CATALOG".to_string());
    }
    Ok(cursor.to_string())
}

async fn discover_pages(
    provider: &ProviderDefinition,
    key: Option<&ApiKey>,
    previous: Option<Catalog>,
) -> Result<Catalog, String> {
    let base = provider
        .base_url
        .as_deref()
        .ok_or_else(|| "PROVIDER_CUSTOM_SETUP_REQUIRED".to_string())?;
    let mut url = if provider.id == "lmstudio" {
        format!("{}/api/v1/models", base.trim_end_matches("/v1"))
    } else {
        format!("{base}{}", provider.models_path)
    };
    let mut models = Vec::new();
    if let Some(previous) = previous {
        if previous.provider_id != provider.id {
            return Err("PROVIDER_INVALID_CATALOG".to_string());
        }
        if let Some(cursor) = previous.next_cursor {
            url = checked_cursor(&url, &cursor)?;
        }
        models = previous.models;
    }
    let initial_count = models.len();
    let mut visited = std::collections::HashSet::new();
    let mut next_cursor = None;
    for _ in 0..20 {
        if !visited.insert(url.clone()) {
            return Err("PROVIDER_INVALID_CATALOG".to_string());
        }
        let payload = get_json(provider, &url, key).await?;
        models.extend(parse_models(provider, &payload)?);
        next_cursor = next_page(provider, &payload, &url);
        match &next_cursor {
            Some(next) if models.len().saturating_sub(initial_count) < 10_000 => url = next.clone(),
            _ => break,
        }
    }
    if provider.id == "ollama" {
        let root = base.trim_end_matches("/v1");
        let running = get_json(provider, &format!("{root}/api/ps"), key)
            .await
            .ok();
        for model in models.iter_mut().take(50) {
            if let Ok(details) = metadata_json(
                provider,
                &format!("{root}/api/show"),
                key,
                Some(&serde_json::json!({"model":model.id})),
            )
            .await
            {
                if let Some(capabilities) = details["capabilities"].as_array() {
                    model.script = Support::from_bool(Some(
                        capabilities
                            .iter()
                            .any(|capability| capability == "completion"),
                    ));
                    model.vision = Support::from_bool(Some(
                        capabilities.iter().any(|capability| capability == "vision"),
                    ));
                }
                model.loaded = running
                    .as_ref()
                    .and_then(|payload| payload["models"].as_array())
                    .is_some_and(|models| {
                        models
                            .iter()
                            .any(|running| running["name"].as_str() == Some(&model.id))
                    });
            }
        }
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    let mut voices: Vec<_> = provider
        .voices
        .iter()
        .map(|id| VoiceDescriptor {
            id: id.clone(),
            name: id.clone(),
            language: None,
        })
        .collect();
    let mut voices_complete = true;
    if matches!(provider.id.as_str(), "cartesia" | "gemini") {
        let path = if provider.id == "cartesia" {
            "/voices?limit=100"
        } else {
            "/v1beta/voices?pageSize=100"
        };
        let mut voice_url = format!("{base}{path}");
        let mut live_voices = Vec::new();
        for page in 0..20 {
            match get_json(provider, &voice_url, key).await {
                Ok(payload) => {
                    let rows = payload["data"]
                        .as_array()
                        .or_else(|| payload["voices"].as_array());
                    let Some(rows) = rows else {
                        voices_complete = false;
                        break;
                    };
                    live_voices.extend(rows.iter().filter_map(|row| {
                        let id = row["id"].as_str().or_else(|| row["name"].as_str())?;
                        Some(VoiceDescriptor {
                            id: id.to_string(),
                            name: row["displayName"]
                                .as_str()
                                .or_else(|| row["name"].as_str())
                                .unwrap_or(id)
                                .to_string(),
                            language: row["language"].as_str().map(str::to_string),
                        })
                    }));
                    match next_page(provider, &payload, &voice_url) {
                        Some(next) if page < 19 => voice_url = next,
                        Some(_) => {
                            voices_complete = false;
                            break;
                        }
                        None => break,
                    }
                }
                Err(_) => {
                    voices_complete = false;
                    break;
                }
            }
        }
        if !live_voices.is_empty() {
            voices = live_voices;
        }
    }
    let recommendations = recommend(provider, &models, &voices);
    let revision = hex::encode(Sha256::digest(
        serde_json::to_vec(&(&models, &voices)).map_err(|_| "PROVIDER_INVALID_CATALOG")?,
    ));
    Ok(Catalog {
        provider_id: provider.id.clone(),
        models,
        voices,
        recommendations,
        configured: Recommendations::default(),
        revision,
        scope: String::new(),
        fetched_unix: crate::storage::now_ms() / 1000,
        complete: next_cursor.is_none(),
        next_cursor,
        voices_complete,
        credential_required: !provider.local || key.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::directory::definition;

    #[test]
    fn a_continuation_cursor_cannot_redirect_credentials() {
        let initial = "https://api.example.test/v1/models";
        assert!(checked_cursor(initial, "https://evil.test/v1/models?pageToken=x").is_err());
        assert!(
            checked_cursor(initial, "https://api.example.test/v1/messages?pageToken=x").is_err()
        );
        assert!(checked_cursor(initial, "https://api.example.test/v1/models?pageToken=x").is_ok());
    }

    #[test]
    fn documented_provider_fixtures_keep_role_and_identity_contracts() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../fixtures/providers/discovery.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let provider = definition(case["providerId"].as_str().unwrap()).unwrap();
            let models = parse_models(provider, &case["payload"]).unwrap();
            let chosen = recommend(provider, &models, &[]);
            assert_eq!(
                chosen.script.as_deref(),
                case["expected"]["script"].as_str(),
                "{}",
                provider.id
            );
            assert_eq!(
                chosen.narration.as_deref(),
                case["expected"]["narration"].as_str(),
                "{}",
                provider.id
            );
        }
    }

    #[test]
    fn model_capabilities_do_not_follow_provider_or_numeric_names() {
        let provider = definition("mistral").unwrap();
        let models = parse_models(
            provider,
            &serde_json::json!({"data":[
                {"id":"embedding-9000","capabilities":{"completion_chat":false,"vision":false}},
                {"id":"mistral-small-2603","capabilities":{"completion_chat":true,"vision":true}}
            ]}),
        )
        .unwrap();
        assert_eq!(
            recommend(provider, &models, &[]).script.as_deref(),
            Some("mistral-small-2603")
        );
        assert_eq!(models[0].script, Support::Unsupported);
        assert_eq!(models[1].vision, Support::Supported);
    }

    #[test]
    fn discovery_keeps_unknowns_but_never_automatically_chooses_them() {
        let provider = definition("openai").unwrap();
        let models = parse_models(provider, &serde_json::json!({"data":[{"id":"mystery-999"},{"id":"gpt-6-luna"},{"id":"gpt-realtime-2.1-mini"}]})).unwrap();
        let recommended = recommend(provider, &models, &[]);
        assert_eq!(models[0].script, Support::Unknown);
        assert_eq!(recommended.script.as_deref(), Some("gpt-6-luna"));
        assert_eq!(
            recommended.narration.as_deref(),
            Some("gpt-realtime-2.1-mini")
        );
    }

    #[test]
    fn pagination_never_follows_a_provider_supplied_host() {
        let provider = definition("gemini").unwrap();
        let next = next_page(
            provider,
            &serde_json::json!({"nextPageToken":"https://evil.test/key"}),
            "https://generativelanguage.googleapis.com/v1beta/models",
        )
        .unwrap();
        assert!(next.starts_with("https://generativelanguage.googleapis.com/"));
        assert!(next.contains("pageToken="));
    }
}
