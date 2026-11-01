//! Registry entries and phase contracts (charter §3.1, §7 entity table).
//!
//! Registered v1 provider kinds: the S-1/S-2 Script set plus the S-3 TTS
//! set (cartesia_tts, openai_tts — Azure is owner-deferred per the A16
//! ruling of 2026-08-28). Each entry records endpoint policy, credential
//! kind, and phase capabilities; capability filtering derives from these
//! facts, never from user-asserted text (charter §4.1).

use async_trait::async_trait;

use crate::voice::keys::Provider as VaultProvider;

/// Provider-capable batch phases (charter §3.1). `VlmDescribe` joins when
/// its slice is scheduled (charter §2.1); absent on purpose until then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseCapability {
    ScriptLlm,
    TtsSynth,
    VisionMllm,
    /// Live Book Agent session (charter B-3b): whole-book voice agent over
    /// the async-tool-capable Gemini native-audio channel.
    #[cfg(feature = "book-agent")]
    AgentLive,
}

/// Endpoint policy per provider kind (charter §4.1, §6.3): fixed-provider
/// URLs are adapter-owned and never editable; only OpenAI-compatible/custom
/// connections carry a user base URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointPolicy {
    Fixed(&'static str),
    CustomRequired,
}

/// Credential requirement (charter A1 safe default): key required
/// everywhere, except OpenAI-compatible connections whose canonical host is
/// exact loopback, which may declare credential-not-required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialPolicy {
    KeyRequired,
    KeyOrLoopbackKeyless,
}

/// The v1 registered provider kinds (charter §2.2 Script set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Generic OpenAI-compatible endpoint: hosted or local, through base URL
    /// plus user-entered model. Quirk profiles are adapter-local and inferred
    /// (A25); no user-facing quirk knob.
    OpenaiCompatible,
    /// Gemini preset on its fixed official host. Protocol choice (official
    /// OpenAI-compatible endpoint vs native generateContent) is documented
    /// adapter-locally in S-1 per A17.
    Gemini,
    /// Native Anthropic Messages adapter (S-2); the OpenAI compatibility
    /// layer is not the production path (charter §2.2).
    Anthropic,
    /// Cartesia TTS on its fixed official host (S-3, owner-ratified
    /// primary): native word timestamps over documented SSE
    /// (`add_timestamps`), normalized into the versioned words envelope.
    CartesiaTts,
    /// OpenAI TTS on its fixed official host (S-3): deliberately
    /// timestamp-less — the universal local-align fallback path (T16).
    OpenaiTts,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 5] = [
        ProviderKind::OpenaiCompatible,
        ProviderKind::Gemini,
        ProviderKind::Anthropic,
        ProviderKind::CartesiaTts,
        ProviderKind::OpenaiTts,
    ];

    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::OpenaiCompatible => "openai_compatible",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::CartesiaTts => "cartesia_tts",
            ProviderKind::OpenaiTts => "openai_tts",
        }
    }

    pub fn from_id(id: &str) -> Option<ProviderKind> {
        ProviderKind::ALL.into_iter().find(|k| k.id() == id)
    }

    pub fn phases(self) -> &'static [PhaseCapability] {
        match self {
            ProviderKind::OpenaiCompatible => {
                &[PhaseCapability::ScriptLlm, PhaseCapability::VisionMllm]
            }
            #[cfg(not(feature = "book-agent"))]
            ProviderKind::Gemini => &[
                PhaseCapability::ScriptLlm,
                PhaseCapability::VisionMllm,
                PhaseCapability::TtsSynth,
            ],
            #[cfg(feature = "book-agent")]
            ProviderKind::Gemini => &[
                PhaseCapability::ScriptLlm,
                PhaseCapability::VisionMllm,
                PhaseCapability::AgentLive,
                PhaseCapability::TtsSynth,
            ],
            ProviderKind::Anthropic => &[PhaseCapability::ScriptLlm, PhaseCapability::VisionMllm],
            ProviderKind::CartesiaTts => &[PhaseCapability::TtsSynth],
            ProviderKind::OpenaiTts => &[PhaseCapability::TtsSynth],
        }
    }

    /// Whether the phase adapter actually exists yet. All three v1 Script
    /// kinds are served since S-2 landed the native Anthropic Messages
    /// adapter; the match stays explicit so a future kind must decide here
    /// rather than inherit servability — a capability nothing can construct
    /// must not reach a picker (T10).
    pub fn adapter_implemented(self) -> bool {
        match self {
            ProviderKind::OpenaiCompatible | ProviderKind::Gemini | ProviderKind::Anthropic => true,
            ProviderKind::CartesiaTts | ProviderKind::OpenaiTts => true,
        }
    }

    /// The phases a connection of this kind can actually serve today.
    /// Cast-servable phases only. AgentLive is a live-session capability,
    /// not a cast phase: it never enters the cast planner's connection views
    /// (the shared IPC fixture contract). NOTE for the Agent UI: enumerate
    /// agent-capable providers via `phases()` (as `authorize_agent_egress`
    /// does), never through this cast-scoped projection or `kinds_for_phase`.
    pub fn servable_phases(self) -> &'static [PhaseCapability] {
        if !self.adapter_implemented() {
            return &[];
        }
        #[cfg(feature = "book-agent")]
        {
            // Generic strip, not a per-provider match: a future second
            // AgentLive provider must not leak into cast views by omission.
            static STRIPPED: std::sync::OnceLock<Vec<Vec<PhaseCapability>>> =
                std::sync::OnceLock::new();
            let stripped = STRIPPED.get_or_init(|| {
                ProviderKind::ALL
                    .iter()
                    .map(|kind| {
                        kind.phases()
                            .iter()
                            .copied()
                            .filter(|p| *p != PhaseCapability::AgentLive)
                            .collect()
                    })
                    .collect()
            });
            let idx = ProviderKind::ALL
                .iter()
                .position(|k| *k == self)
                .expect("every kind is in ALL");
            stripped[idx].as_slice()
        }
        #[cfg(not(feature = "book-agent"))]
        {
            self.phases()
        }
    }

    pub fn endpoint_policy(self) -> EndpointPolicy {
        match self {
            ProviderKind::OpenaiCompatible => EndpointPolicy::CustomRequired,
            ProviderKind::Gemini => {
                EndpointPolicy::Fixed("https://generativelanguage.googleapis.com")
            }
            ProviderKind::Anthropic => EndpointPolicy::Fixed("https://api.anthropic.com"),
            ProviderKind::CartesiaTts => EndpointPolicy::Fixed("https://api.cartesia.ai"),
            ProviderKind::OpenaiTts => EndpointPolicy::Fixed("https://api.openai.com"),
        }
    }

    pub fn credential_policy(self) -> CredentialPolicy {
        match self {
            ProviderKind::OpenaiCompatible => CredentialPolicy::KeyOrLoopbackKeyless,
            ProviderKind::Gemini
            | ProviderKind::Anthropic
            | ProviderKind::CartesiaTts
            | ProviderKind::OpenaiTts => CredentialPolicy::KeyRequired,
        }
    }

    /// The adapter's contract revision (T18 snapshot field): bumped when an
    /// adapter's wire behavior changes meaningfully; resume refuses a
    /// binding recorded under a different revision rather than replaying it
    /// against changed semantics.
    pub fn adapter_revision(self) -> &'static str {
        match self {
            ProviderKind::OpenaiCompatible => "openai_compat/1",
            ProviderKind::Gemini => "gemini/1",
            ProviderKind::Anthropic => "anthropic/1",
            ProviderKind::CartesiaTts => "cartesia_tts/1",
            ProviderKind::OpenaiTts => "openai_tts/1",
        }
    }

    pub fn narration_revision(self, model: &str) -> &'static str {
        match self {
            Self::OpenaiTts if model.starts_with("gpt-realtime-") => "openai_realtime_tts/1",
            Self::Gemini if model.starts_with("gemini-3.8-") && model.contains("tts") => {
                "gemini_tts/2"
            }
            _ => self.adapter_revision(),
        }
    }

    /// The exhaustive canonical destination host set a connection of this
    /// kind may contact (charter A14) — what consent names, and what the
    /// egress disclosure shows. Fixed kinds declare their official host;
    /// custom kinds derive from the connection's canonical base URL.
    pub fn declared_hosts(self, base_url: Option<&str>) -> Vec<String> {
        match self.endpoint_policy() {
            EndpointPolicy::Fixed(url) => vec![reqwest::Url::parse(url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .unwrap_or_else(|| url.to_string())],
            EndpointPolicy::CustomRequired => base_url
                .and_then(|url| reqwest::Url::parse(url).ok())
                .and_then(|url| url.host_str().map(str::to_string))
                .map(|host| vec![host])
                .unwrap_or_default(),
        }
    }

    /// The exact data categories each phase sends (A13): what consent copy
    /// names, verbatim.
    pub fn data_categories(phase: PhaseCapability) -> Vec<String> {
        match phase {
            PhaseCapability::ScriptLlm => vec!["source_text".to_string()],
            PhaseCapability::TtsSynth => vec!["script_text".to_string()],
            PhaseCapability::VisionMllm => vec!["figure_crops".to_string()],
            // Charter §6.4/A5: the Agent's exact canonical category set —
            // `source_text` reused for every book-derived excerpt (snippets,
            // opened passages, AND the recognition metadata block: consent
            // copy says "book text and book metadata"), plus the voice
            // session's own egress. Single enumeration site (T13).
            #[cfg(feature = "book-agent")]
            // F7 (owner-ratified copy change, 2026-08-30): the Agent also
            // sends rendered PAGE IMAGES — whole-page stills carrying every
            // visual element (figures, charts, diagrams, tables, rendered
            // equations) — proactively on page changes. A5 requires the
            // consent copy to name it, so it is a category, not a footnote;
            // existing consents lack it and re-prompt by construction
            // (find_valid matches the full category set).
            PhaseCapability::AgentLive => vec![
                "source_text".to_string(),
                "voice_audio".to_string(),
                "voice_transcript".to_string(),
                "reading_position".to_string(),
                "page_images".to_string(),
            ],
        }
    }

    /// Provider retention/privacy disclosure link for consent copy (A13).
    /// Custom endpoints have no single truth — the copy says so instead.
    pub fn retention_url(self) -> Option<&'static str> {
        match self {
            ProviderKind::OpenaiCompatible => None,
            ProviderKind::Gemini => Some("https://ai.google.dev/gemini-api/terms"),
            ProviderKind::Anthropic => Some("https://www.anthropic.com/legal/privacy"),
            ProviderKind::CartesiaTts => Some("https://cartesia.ai/legal/privacy"),
            ProviderKind::OpenaiTts => {
                Some("https://developers.openai.com/api/docs/guides/your-data")
            }
        }
    }

    /// The VA3 custody row this kind's keys live under — one shared secret
    /// store, never a second one (charter §4.2).
    pub fn vault_provider(self) -> VaultProvider {
        match self {
            ProviderKind::OpenaiCompatible => VaultProvider::OpenaiCompat,
            ProviderKind::Gemini => VaultProvider::Gemini,
            ProviderKind::Anthropic => VaultProvider::Anthropic,
            // Cartesia custody shares the row the Voice Tutor already uses;
            // OpenAI TTS keys live under the OpenAI-compatible row — custody
            // is scoped by canonical base URL either way (charter §4.2, one
            // shared secret store, never a second one).
            ProviderKind::CartesiaTts => VaultProvider::Cartesia,
            ProviderKind::OpenaiTts => VaultProvider::OpenaiCompat,
        }
    }
}

/// Provider-neutral cancellation signal for batch phase work (S-3b, Hermes
/// MF11): the orchestrator holds the handle bridged from its job control;
/// contract code (`teach_chunks`) races provider awaits against the token.
/// Watch-based so `cancelled()` is a reusable, race-safe future; a dropped
/// handle reads as cancelled (an orphaned run must not keep paying).
pub struct CancelHandle {
    tx: tokio::sync::watch::Sender<bool>,
}

#[derive(Clone)]
pub struct CancelToken {
    rx: tokio::sync::watch::Receiver<bool>,
}

pub fn cancel_channel() -> (CancelHandle, CancelToken) {
    let (tx, rx) = tokio::sync::watch::channel(false);
    (CancelHandle { tx }, CancelToken { rx })
}

impl CancelHandle {
    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }
}

impl CancelToken {
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    pub async fn cancelled(&mut self) {
        if *self.rx.borrow() {
            return;
        }
        while self.rx.changed().await.is_ok() {
            if *self.rx.borrow() {
                return;
            }
        }
        // Sender dropped: treat as cancelled rather than pending forever.
    }

    /// A token that never fires — for callers with no cancellation source
    /// (tests, contexts that cannot be interrupted).
    pub fn never() -> CancelToken {
        let (handle, token) = cancel_channel();
        std::mem::forget(handle);
        token
    }
}

/// The typed provider-failure projection (Hermes MF10, charter §5.5):
/// class, retryability, retry deadline, and bounded safe detail — merged
/// into the job report so recovery UI reasons from facts, not strings.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderFailure {
    pub class: String,
    pub retryable: bool,
    pub retry_after_secs: Option<u64>,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

impl ProviderFailure {
    pub fn from_script_error(error: &ScriptError) -> ProviderFailure {
        let detail = if error.kind == ScriptErrorKind::Auth {
            match error.status {
                Some(s) => format!("authentication failed (HTTP {s})"),
                None => "authentication failed".to_string(),
            }
        } else {
            super::adapters::sanitize_error_message(&error.message)
                .chars()
                .take(300)
                .collect()
        };
        ProviderFailure {
            class: match error.kind {
                ScriptErrorKind::Auth => "auth",
                ScriptErrorKind::Quota => "quota",
                ScriptErrorKind::Transport => "transport",
                ScriptErrorKind::Timeout => "timeout",
                ScriptErrorKind::Provider => "provider",
                ScriptErrorKind::ModelNotFound => "model_not_found",
                ScriptErrorKind::Malformed => "malformed",
                ScriptErrorKind::Cancelled => "cancelled",
            }
            .to_string(),
            retryable: error.retryable,
            retry_after_secs: error.retry_after_secs,
            detail,
            status: error.status,
        }
    }
}

use serde::Serialize;

pub fn kinds_for_phase(phase: PhaseCapability) -> Vec<ProviderKind> {
    ProviderKind::ALL
        .into_iter()
        .filter(|kind| kind.servable_phases().contains(&phase))
        .collect()
}

/// Provider-neutral usage facts a Script call may report (charter §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScriptUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptResponse {
    pub text: String,
    pub usage: ScriptUsage,
}

/// Classified Script failure kinds mirroring charter §5.5's taxonomy —
/// remedies differ, so classes stay distinct and transport never implies an
/// invalid key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptErrorKind {
    Auth,
    Quota,
    Transport,
    Timeout,
    Provider,
    ModelNotFound,
    Malformed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    pub kind: ScriptErrorKind,
    pub retryable: bool,
    pub message: String,
    pub retry_after_secs: Option<u64>,
    pub status: Option<u16>,
}

/// One Script-contract generation request. The script contract (chunking,
/// summaries, repair ladder) lives above this trait; adapters own fixed
/// internal decoding values (charter §3.4) — nothing here is a user knob.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptRequest {
    pub system: String,
    pub user: String,
    pub max_output_tokens: u32,
    /// Fixed internal decoding value owned by the script contract (0.0
    /// first attempt, 0.3 repair attempts — teacher.py verbatim). Never a
    /// user knob (charter §3.4).
    pub temperature: f32,
    /// Reasoning/thinking depth (owner ruling 2026-08-29): the default is
    /// the provider's LOW tier, always. This is a pass-through seam for a
    /// future per-phase override (F6), never a UI knob on this surface;
    /// each adapter maps it to its provider's documented parameter and
    /// omits it where the model/host does not document one.
    pub effort: ReasoningEffort,
}

/// The provider-neutral effort ladder. `Low` is the owner-ruled default
/// (2026-08-29): assume the user knows nothing — the assembled request is
/// the efficient one unless an explicit override arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    #[default]
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    /// The lowercase wire word all three verified provider parameters use
    /// (Gemini thinkingLevel, Anthropic output_config.effort, OpenAI
    /// reasoning_effort — docs cited at each adapter's mapping site).
    pub fn wire(self) -> &'static str {
        match self {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        }
    }
}

/// The Script-LLM phase contract (charter §3.1). Object-safe; one
/// implementation is chosen at the binding point and held as
/// `Box<dyn ScriptLlm>`.
#[async_trait]
pub trait ScriptLlm: Send {
    async fn generate(&mut self, request: ScriptRequest) -> Result<ScriptResponse, ScriptError>;
}

/// The TTS phase contract shape (charter §3.1); implementations land in S-3.
#[async_trait]
pub trait TtsSynth: Send {
    async fn synthesize(&mut self, text: &str, voice: &str) -> Result<TtsArtifact, ScriptError>;
    async fn cancel(&mut self) {}
}

/// Provider-neutral TTS outcome: an audio artifact path plus optional
/// normalized native word timing (charter §3.5). Timing is an optimization,
/// never an eligibility gate.
#[derive(Debug, Clone, PartialEq)]
pub struct TtsArtifact {
    pub audio_path: std::path::PathBuf,
    pub native_words: Option<serde_json::Value>,
}

/// What a phase binding resolves to (charter §4.4): the local sidecar path,
/// or a named connection. Configured cloud connections never enter automatic
/// resolution; absent an explicit binding, the answer is Local.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Local {
        model_id: Option<String>,
    },
    Connection {
        connection_id: String,
        model: String,
    },
}

pub fn resolve_phase(binding: Option<&crate::providers::PhaseBinding>) -> Resolution {
    match binding {
        None => Resolution::Local { model_id: None },
        Some(crate::providers::PhaseBinding::Local { model_id }) => Resolution::Local {
            model_id: Some(model_id.clone()),
        },
        Some(crate::providers::PhaseBinding::Connection {
            connection_id,
            model,
            ..
        }) => Resolution::Connection {
            connection_id: connection_id.clone(),
            model: model.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_script_llm_object_safe(_: &dyn ScriptLlm) {}
    fn assert_tts_object_safe(_: &dyn TtsSynth) {}

    struct FakeScript;
    #[async_trait]
    impl ScriptLlm for FakeScript {
        async fn generate(
            &mut self,
            _request: ScriptRequest,
        ) -> Result<ScriptResponse, ScriptError> {
            Ok(ScriptResponse {
                text: "ok".to_string(),
                usage: ScriptUsage::default(),
            })
        }
    }

    struct FakeTts;
    #[async_trait]
    impl TtsSynth for FakeTts {
        async fn synthesize(
            &mut self,
            _text: &str,
            _voice: &str,
        ) -> Result<TtsArtifact, ScriptError> {
            Ok(TtsArtifact {
                audio_path: std::path::PathBuf::from("/dev/null"),
                native_words: None,
            })
        }
    }

    #[test]
    fn the_phase_traits_are_object_safe() {
        assert_script_llm_object_safe(&FakeScript);
        assert_tts_object_safe(&FakeTts);
    }

    #[test]
    fn the_registry_is_capability_filtered_per_phase() {
        assert_eq!(
            kinds_for_phase(PhaseCapability::ScriptLlm),
            vec![
                ProviderKind::OpenaiCompatible,
                ProviderKind::Gemini,
                ProviderKind::Anthropic
            ],
            "all three v1 script adapters are shipped as of S-2"
        );
        assert_eq!(
            kinds_for_phase(PhaseCapability::TtsSynth),
            vec![
                ProviderKind::Gemini,
                ProviderKind::CartesiaTts,
                ProviderKind::OpenaiTts
            ],
            "S-3 ships Cartesia (native timing) and OpenAI TTS (align \
             fallback); Gemini TTS; Azure is owner-deferred"
        );
        assert_eq!(
            kinds_for_phase(PhaseCapability::VisionMllm),
            vec![
                ProviderKind::OpenaiCompatible,
                ProviderKind::Gemini,
                ProviderKind::Anthropic
            ],
            "vision MLLM is supported by OpenAI-compatible, Gemini, and Anthropic"
        );
    }

    #[test]
    fn ids_round_trip_and_endpoint_policy_matches_charter() {
        for kind in ProviderKind::ALL {
            assert_eq!(ProviderKind::from_id(kind.id()), Some(kind));
        }
        assert_eq!(
            ProviderKind::OpenaiCompatible.endpoint_policy(),
            EndpointPolicy::CustomRequired
        );
        assert!(matches!(
            ProviderKind::Gemini.endpoint_policy(),
            EndpointPolicy::Fixed(url) if url.starts_with("https://")
        ));
        assert!(matches!(
            ProviderKind::Anthropic.endpoint_policy(),
            EndpointPolicy::Fixed("https://api.anthropic.com")
        ));
    }

    #[test]
    fn unbound_phases_resolve_local_and_bindings_resolve_verbatim() {
        assert_eq!(resolve_phase(None), Resolution::Local { model_id: None });
        let local = crate::providers::PhaseBinding::Local {
            model_id: "phi-4-mini".to_string(),
        };
        assert_eq!(
            resolve_phase(Some(&local)),
            Resolution::Local {
                model_id: Some("phi-4-mini".to_string())
            }
        );
        let cloud = crate::providers::PhaseBinding::Connection {
            provider: "openai_compatible".to_string(),
            endpoint: "http://127.0.0.1:11434/v1".to_string(),
            adapter_rev: "openai_compat/1".to_string(),
            connection_id: "conn-1".to_string(),
            model: "some-model".to_string(),
            effort: ReasoningEffort::Low,
        };
        assert_eq!(
            resolve_phase(Some(&cloud)),
            Resolution::Connection {
                connection_id: "conn-1".to_string(),
                model: "some-model".to_string()
            }
        );
    }
}
