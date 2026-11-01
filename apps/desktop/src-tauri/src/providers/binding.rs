//! Per-job phase binding schema (charter §4.4): a binding distinguishes a
//! local model id from a connection id + model override structurally —
//! never an ambiguous bare string. Jobs persist this; resume/retry replays
//! the recorded binding verbatim and never re-resolves against changed
//! settings (T18).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PhaseBinding {
    Local {
        model_id: String,
    },
    Connection {
        connection_id: String,
        model: String,
        /// T18 snapshot (Hermes pre-brief item 1): provider kind, canonical
        /// endpoint, and adapter revision as they stood at cast time.
        /// Readers ENFORCE these against the live connection — a rebound
        /// base URL or changed adapter refuses instead of silently
        /// redirecting unfinished work.
        provider: String,
        endpoint: String,
        adapter_rev: String,
        /// Per-phase reasoning effort recorded at cast time (F6 override,
        /// owner ruling 2026-08-29: LOW default). Absent in older recorded
        /// bindings — those replay at the ruled default.
        #[serde(default)]
        effort: crate::providers::registry::ReasoningEffort,
    },
}

/// The TTS phase binding (S-3, A15 ruling of 2026-08-28): a cloud binding
/// records the resolved {connection, model, voice} tuple copied from the
/// selected voice profile AT CAST TIME. The profile is the selection
/// surface; this is the recorded truth — resume reads only the binding and
/// never re-resolves against the (mutable) profile or connection (T18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TtsBinding {
    Local {
        model_id: String,
    },
    Connection {
        connection_id: String,
        model: String,
        voice: String,
        provider: String,
        endpoint: String,
        adapter_rev: String,
    },
}

/// A cloud voice profile's engine value (A15 Option 3): provider identity
/// stays on the connection, never in profile rows (charter §3.3 law 2).
pub const CLOUD_VOICE_ENGINE: &str = "cloud";

/// The structural connection reference inside a cloud profile's
/// `params_json` — the same `connection_id` key the delete-impact JSON walk
/// finds, so deleting a connection names its stranded profiles.
pub fn cloud_profile_connection_id(params_json: Option<&str>) -> Option<String> {
    let raw = params_json?;
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value
        .get("connection_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// The ONE canonical cloud-profile validator (Hermes R2 cloud-profile
/// ruling 1): every consumer — options listing, the cast writer, the A22
/// advisory — goes through this, so a malformed or stranded cloud profile
/// has one refusal shape everywhere.
pub struct ValidCloudVoice {
    pub voice_id: String,
    pub connection_id: String,
}

pub fn validate_cloud_profile(
    engine: &str,
    kind: &str,
    reference_wav_path: Option<&str>,
    preset_voice_id: Option<&str>,
    params_json: Option<&str>,
) -> Result<ValidCloudVoice, String> {
    if engine != CLOUD_VOICE_ENGINE {
        return Err("not a cloud voice profile".to_string());
    }
    if kind != "preset" {
        return Err("a cloud voice is a provider preset; this profile's kind is wrong".to_string());
    }
    if reference_wav_path.is_some() {
        return Err("a cloud voice carries no local recording".to_string());
    }
    let voice_id = preset_voice_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| "this cloud voice names no provider voice id".to_string())?;
    let connection_id = cloud_profile_connection_id(params_json).ok_or_else(|| {
        "this cloud voice no longer names a connection; re-create it in Providers".to_string()
    })?;
    Ok(ValidCloudVoice {
        voice_id: voice_id.to_string(),
        connection_id,
    })
}

/// Cast-time resolution (A15): copy the tuple out of the selection surface
/// into the recorded binding. `voice` is the profile's `preset_voice_id`
/// (the provider voice identifier); `model` is the connection's
/// default_model as it stands at cast time. After this call the job is
/// independent of both rows.
pub fn tts_binding_from_cloud_profile(
    profile_preset_voice_id: Option<&str>,
    connection: &crate::providers::ConnectionRecord,
) -> Result<TtsBinding, String> {
    let connection_id = connection.id.as_str();
    let connection_default_model = connection.default_model.as_str();
    let voice = profile_preset_voice_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            "a cloud voice profile must carry the provider voice id in preset_voice_id; \
             no provider voice is ever guessed (A15)"
                .to_string()
        })?;
    let model = connection_default_model.trim();
    if model.is_empty() {
        return Err("the connection has no default model to record".to_string());
    }
    let voice = if connection.provider == crate::providers::ProviderKind::Gemini
        && !model.starts_with("gemini-3.8-")
    {
        let canonical = crate::providers::adapters::gemini_tts::normalize_voice_name(voice);
        if !canonical.eq_ignore_ascii_case(voice) {
            return Err("this voice requires a current Gemini narration model; customize models or choose a documented prebuilt voice".to_string());
        }
        canonical
    } else {
        voice
    };
    Ok(TtsBinding::Connection {
        connection_id: connection_id.to_string(),
        model: model.to_string(),
        voice: voice.to_string(),
        provider: connection.provider.id().to_string(),
        endpoint: connection.key_base_url(),
        adapter_rev: connection.provider.narration_revision(model).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_gemini_bindings_record_the_voice_supported_by_the_recorded_speech_revision() {
        let mut connection =
            crate::providers::ConnectionRecord::draft("gemini", None, false).unwrap();
        connection.default_model = "gemini-3.1-flash-tts-preview".to_string();
        assert!(tts_binding_from_cloud_profile(Some("voice_library_1"), &connection).is_err());
        let legacy = tts_binding_from_cloud_profile(Some("kore"), &connection).unwrap();
        assert!(matches!(legacy, TtsBinding::Connection { voice, .. } if voice == "Kore"));
        connection.default_model = "gemini-3.8-flash-lite-tts".to_string();
        let current = tts_binding_from_cloud_profile(Some("voice_library_1"), &connection).unwrap();
        assert!(
            matches!(current, TtsBinding::Connection { voice, adapter_rev, .. } if voice == "voice_library_1" && adapter_rev == "gemini_tts/2")
        );
    }

    #[test]
    fn tts_bindings_round_trip_and_reject_ambiguity() {
        let cloud = TtsBinding::Connection {
            connection_id: "c-1".to_string(),
            model: "sonic-3.6".to_string(),
            voice: "db6b0ed5-d5d3-463d-ae85-518a07d3c2b4".to_string(),
            provider: "cartesia_tts".to_string(),
            endpoint: "https://api.cartesia.ai".to_string(),
            adapter_rev: "cartesia_tts/1".to_string(),
        };
        let json = serde_json::to_string(&cloud).unwrap();
        assert!(json.contains("\"kind\":\"connection\""));
        assert_eq!(serde_json::from_str::<TtsBinding>(&json).unwrap(), cloud);
        assert!(serde_json::from_str::<TtsBinding>("\"kokoro-82m\"").is_err());
        assert!(
            serde_json::from_str::<TtsBinding>(
                "{\"kind\":\"connection\",\"connection_id\":\"c\",\"model\":\"m\",\"voice\":\"v\"}"
            )
            .is_err(),
            "a binding without its T18 snapshot fields is not a binding"
        );
    }

    #[test]
    fn cast_time_copy_takes_the_tuple_and_refuses_guessing() {
        let connection = crate::providers::ConnectionRecord {
            id: "c-9".to_string(),
            name: "OpenAI TTS".to_string(),
            provider: crate::providers::ProviderKind::OpenaiTts,
            base_url: None,
            credential_not_required: false,
            default_model: "gpt-4o-mini-tts".to_string(),
            enabled: true,
            created_unix: 0,
            updated_unix: 0,
        };
        let binding = tts_binding_from_cloud_profile(Some("alloy"), &connection).expect("copies");
        assert_eq!(
            binding,
            TtsBinding::Connection {
                connection_id: "c-9".to_string(),
                model: "gpt-4o-mini-tts".to_string(),
                voice: "alloy".to_string(),
                provider: "openai_tts".to_string(),
                endpoint: "https://api.openai.com".to_string(),
                adapter_rev: "openai_tts/1".to_string(),
            }
        );
        assert!(tts_binding_from_cloud_profile(None, &connection).is_err());
        assert!(tts_binding_from_cloud_profile(Some("  "), &connection).is_err());
        let blank_model = crate::providers::ConnectionRecord {
            default_model: " ".to_string(),
            ..connection
        };
        assert!(tts_binding_from_cloud_profile(Some("alloy"), &blank_model).is_err());
    }

    #[test]
    fn cloud_profile_connection_reference_is_structural() {
        assert_eq!(
            cloud_profile_connection_id(Some(r#"{"connection_id": "c-1"}"#)),
            Some("c-1".to_string())
        );
        assert_eq!(cloud_profile_connection_id(Some("not json")), None);
        assert_eq!(cloud_profile_connection_id(None), None);
    }

    #[test]
    fn bindings_round_trip_with_explicit_kind_tags() {
        let local = PhaseBinding::Local {
            model_id: "phi-4-mini".to_string(),
        };
        let json = serde_json::to_string(&local).unwrap();
        assert!(json.contains("\"kind\":\"local\""));
        assert_eq!(serde_json::from_str::<PhaseBinding>(&json).unwrap(), local);

        let cloud = PhaseBinding::Connection {
            connection_id: "c-1".to_string(),
            model: "big-model".to_string(),
            provider: "openai_compatible".to_string(),
            endpoint: "http://127.0.0.1:11434/v1".to_string(),
            adapter_rev: "openai_compat/1".to_string(),
            effort: crate::providers::registry::ReasoningEffort::Medium,
        };
        let json = serde_json::to_string(&cloud).unwrap();
        assert!(json.contains("\"kind\":\"connection\""));
        assert!(json.contains("\"effort\":\"medium\""));
        assert_eq!(serde_json::from_str::<PhaseBinding>(&json).unwrap(), cloud);
    }

    #[test]
    fn a_recorded_binding_without_effort_replays_at_the_ruled_low_default() {
        let parsed = serde_json::from_str::<PhaseBinding>(
            r#"{"kind":"connection","connection_id":"c-1","model":"m","provider":"gemini","endpoint":"e","adapter_rev":"gemini/1"}"#,
        )
        .unwrap();
        let PhaseBinding::Connection { effort, .. } = parsed else {
            panic!("expected a connection binding");
        };
        assert_eq!(effort, crate::providers::registry::ReasoningEffort::Low);
    }

    #[test]
    fn ambiguous_shapes_are_rejected_not_guessed() {
        assert!(serde_json::from_str::<PhaseBinding>("\"phi-4-mini\"").is_err());
        assert!(serde_json::from_str::<PhaseBinding>("{\"model_id\":\"x\"}").is_err());
        assert!(serde_json::from_str::<PhaseBinding>(
            "{\"kind\":\"local\",\"model_id\":\"x\",\"temperature\":0.7}"
        )
        .is_err());
        assert!(serde_json::from_str::<PhaseBinding>(
            "{\"kind\":\"connection\",\"connection_id\":\"c\"}"
        )
        .is_err());
    }
}
