//! The cloud TTS wired path (provider-flexibility charter S-3, A15 ruling
//! of 2026-08-28; the S-5 dev flag retirement made per-connection
//! enablement the release gate).
//!
//! A job whose `model_overrides_json` carries a structural `"tts_binding"`
//! `TtsBinding` resolves here: each pending segment is synthesized through
//! the recorded connection's adapter, the audio lands in the ordinary wav
//! cache under its content hash (A23: bounded by the adapter, atomically
//! materialized, registered in the cache ledger — no full-book bytes cross
//! IPC), and native word timing is admitted into the versioned words
//! envelope only when the whole set passes the §3.5 gate — otherwise the
//! WHOLE segment falls back to the local align phase (A21; sources are
//! never mixed). Keys resolve from VA3 custody only inside the adapter
//! build (charter §3.3 law 3).
//!
//! The profile was the selection surface; this module reads ONLY the
//! recorded binding — resume never re-resolves against edited profiles or
//! connections (T18, the S-1d proof pattern).

//! Category A Doc Pass (ai.google.dev / 2026-09-14):
//! - Rate limits & quota semantics: https://ai.google.dev/gemini-api/docs/rate-limits
//! - API errors and troubleshooting: https://ai.google.dev/gemini-api/docs/troubleshooting
//! - Rate limit tiers & metrics: https://aistudio.google.com/rate-limit
//!
//! Findings:
//! 1. 429 RESOURCE_EXHAUSTED spans two distinct classes:
//!    - RPM/TPM: short-term sliding window (< 60s), resolved via backoff and auto-resume.
//!    - RPD/Spend: daily request quotas (resets midnight PT) and spending/credit limits;
//!      backoff will not resolve without user intervention (key rotation/billing).
//! 2. Error payloads distinguish daily/spend caps by metric indicators (e.g. 'perday',
//!    'daily', 'spend', 'credit', 'billing tier') or excessive retry-after (> 300s).

use std::path::Path;
use std::sync::Arc;

use sha2::Digest;

use super::overload::{friendly_provider_error, PatienceConfig, PAUSED_KIND_SERVICE_BUSY};
use super::provider_error_policy::{FaultClass, PatienceBudget, ProviderErrorPolicy};
use super::scripting_cloud::{provider_refusal, script_error_code, MAX_OVERLOAD_PAUSE_ATTEMPTS};
use super::stage::{StageContext, StageOutcome};
use super::state::{OrchestratorError, PausedReason, RateLimitClass};
use crate::providers::registry::{ProviderKind, ScriptError, TtsSynth};
use crate::providers::tts_words::validate_native_words;
use crate::providers::{ConnectionRecord, TtsBinding};
use crate::render::mp3::cache_wav_path;
use crate::storage::{job, now_ms};
use crate::voice::keys::{ApiKey, KeyManager};
use crate::words::{Timebase, WordsJson};

/// Maximum backoff sleep duration allowed when all keys in pool hit quota limits.
pub const MAX_QUOTA_WAIT_SECS: u64 = 180;
/// Maximum number of quota backoff waits per segment before returning an engine error.
pub const MAX_QUOTA_BACKOFF_RETRIES: u32 = 6;
/// Default quota cooldown in seconds when provider omits Retry-After / retryDelay.
pub const DEFAULT_QUOTA_BACKOFF_SECS: u64 = 20;
/// Maximum number of retries/rotations for transient retryable provider errors
/// (e.g. empty inline audio, network blip) before failing the segment.
/// Owner-tunable; setting too high delays failure reporting on systematic failures.
pub const MAX_RETRYABLE_RETRIES: u32 = 2;
pub const MAX_QUOTA_PAUSE_ATTEMPTS: u32 = 5;

/// Maximum wait time for auto-resuming per-minute rate limits. If Retry-After exceeds
/// this threshold, the limit is classified as Daily requiring user action.
pub const MAX_AUTO_RESUME_WAIT_SECS: u64 = 300;
/// Minimum wait time for auto-resuming per-minute rate limits.
pub const MIN_AUTO_RESUME_WAIT_SECS: u64 = 20;
/// Default wait time for per-minute rate limits when Retry-After is absent or zero.
pub const DEFAULT_AUTO_RESUME_WAIT_SECS: u64 = 60;

pub fn classify_quota_error(error: &ScriptError) -> RateLimitClass {
    let lower = error.message.to_lowercase();
    let is_daily_indicator = lower.contains("perday")
        || lower.contains("per_day")
        || lower.contains("daily")
        || lower.contains("requests per day")
        || lower.contains("rpd")
        || lower.contains("spend")
        || lower.contains("credit")
        || lower.contains("billing tier")
        || lower.contains("free tier limit");

    if is_daily_indicator {
        return RateLimitClass::Daily;
    }

    if let Some(wait_secs) = error.retry_after_secs {
        if wait_secs > MAX_AUTO_RESUME_WAIT_SECS {
            return RateLimitClass::Daily;
        }
    }

    RateLimitClass::PerMinute
}

pub fn sanitize_error_message(msg: &str) -> String {
    crate::providers::adapters::sanitize_error_message(msg)
}

/// The custody account a stage's key came from, when one was dialed: the
/// seam that classifies provider errors feeds the settled outcome back into
/// slot state through it (S-5 blocker 5 — rotation learns outcomes).
pub type PoolOutcomeSink<'a> = Option<(&'a KeyManager, &'a str)>;

/// The structural per-job TTS binding under `"tts_binding"` — same parse
/// law as the Script binding: an unparseable document carries no binding,
/// a present-but-malformed binding refuses loudly (T18).
pub fn override_tts_binding(raw: Option<&str>) -> Result<Option<TtsBinding>, OrchestratorError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(None);
    };
    let Some(binding) = value.get("tts_binding") else {
        return Ok(None);
    };
    serde_json::from_value::<TtsBinding>(binding.clone())
        .map(Some)
        .map_err(|err| {
            OrchestratorError::InvalidRequest(format!(
                "this job's recorded tts_binding does not parse ({err}); refusing to \
                 re-resolve a recorded binding — fix or clear the binding explicitly"
            ))
        })
}

pub fn create_tts_adapter(
    connection: &ConnectionRecord,
    model: &str,
    staging_dir: std::path::PathBuf,
    key: ApiKey,
) -> Result<Box<dyn TtsSynth>, OrchestratorError> {
    let adapter: Box<dyn TtsSynth> = match connection.provider {
        ProviderKind::CartesiaTts => {
            let res = crate::providers::adapters::cartesia_tts::CartesiaTts::with_base_url(
                connection.key_base_url(),
                key,
                model.to_string(),
                staging_dir,
            );
            Box::new(res.map_err(|err| provider_refusal(err.message))?)
        }
        ProviderKind::OpenaiTts => {
            if model.starts_with("gpt-realtime-") {
                return Ok(Box::new(crate::providers::adapters::openai_realtime_tts::OpenaiRealtimeTts::with_base_url(
                    connection.key_base_url(), key, model.to_string(), staging_dir,
                ).map_err(|error| provider_refusal(error.message))?));
            }
            let res = crate::providers::adapters::openai_tts::OpenaiTts::with_base_url(
                connection.key_base_url(),
                key,
                model.to_string(),
                staging_dir,
            );
            Box::new(res.map_err(|err| provider_refusal(err.message))?)
        }
        ProviderKind::Gemini => {
            let res = crate::providers::adapters::gemini_tts::GeminiTts::with_base_url(
                connection.key_base_url(),
                key,
                model.to_string(),
                staging_dir,
            );
            Box::new(res.map_err(|err| provider_refusal(err.message))?)
        }
        other => {
            return Err(provider_refusal(format!(
                "provider '{}' cannot serve the TTS phase",
                other.id()
            )))
        }
    };
    Ok(adapter)
}

enum BuildAdapterOutcome {
    Ready(Box<dyn TtsSynth>, String),
    Paused(PausedReason),
}

pub async fn read_prior_attempt(ctx: Option<&StageContext>) -> u32 {
    let Some(ctx) = ctx else { return 0 };
    if let Some(reason) = ctx.checkpoint.paused_reason.as_ref() {
        return reason.attempt;
    }
    if let Ok(Some(record)) = job::get_job(&ctx.pool, &ctx.job_id).await {
        if let Some(report_str) = record.report_json.as_deref() {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(report_str) {
                if let Some(attempt) = val
                    .get("paused_reason")
                    .and_then(|r| r.get("attempt"))
                    .and_then(|a| a.as_u64())
                {
                    return attempt as u32;
                }
            }
        }
    }
    0
}

pub async fn strip_report_paused_reason(
    pool: &sqlx::SqlitePool,
    job_id: &str,
) -> Result<(), OrchestratorError> {
    let Some(record) = job::get_job(pool, job_id).await? else {
        return Ok(());
    };
    let mut report: serde_json::Value = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    if report
        .as_object_mut()
        .is_some_and(|object| object.remove("paused_reason").is_some())
    {
        job::set_job_report_only(pool, job_id, &serde_json::to_string(&report)?).await?;
    }
    Ok(())
}

async fn build_tts_adapter(
    pool: &sqlx::SqlitePool,
    keys: Option<&Arc<KeyManager>>,
    connection: &ConnectionRecord,
    model: &str,
    staging_dir: std::path::PathBuf,
    ctx: Option<&StageContext>,
) -> Result<BuildAdapterOutcome, OrchestratorError> {
    let manager = match keys {
        Some(m) => m,
        None => {
            let msg = "key custody is unavailable in this build; a keyed cloud binding cannot run"
                .to_string();
            if let Some(ctx) = ctx {
                let projection = crate::providers::registry::ProviderFailure {
                    class: "auth".to_string(),
                    retryable: false,
                    retry_after_secs: None,
                    detail: msg.clone(),
                    status: None,
                };
                if let Ok(json) = serde_json::to_value(&projection) {
                    let _ = ctx.merge_report_value("provider_failure", json).await;
                }
            }
            return Err(provider_refusal(msg));
        }
    };

    let provider_id = connection.provider.vault_provider().id();
    let base_url = connection.key_base_url();
    let candidate = match manager.resolve_pool_key(pool, provider_id, &base_url).await {
        Ok(c) => c,
        Err(err) => {
            let assessment = manager
                .pool_quota_assessment(pool, Some(provider_id), &base_url)
                .await
                .unwrap_or(crate::voice::keys::PoolQuotaAssessment {
                    total: 0,
                    out_of_quota: 0,
                    invalid: 0,
                    earliest_retry_wait_secs: None,
                    earliest_retry_at: None,
                });

            if assessment.total > 0 && assessment.invalid == assessment.total {
                let detail = format!(
                    "all {} configured keys for connection '{}' are invalid",
                    assessment.total, connection.name
                );
                if let Some(ctx) = ctx {
                    let projection = crate::providers::registry::ProviderFailure {
                        class: "auth".to_string(),
                        retryable: false,
                        retry_after_secs: None,
                        detail: detail.clone(),
                        status: Some(401),
                    };
                    if let Ok(json) = serde_json::to_value(&projection) {
                        let _ = ctx.merge_report_value("provider_failure", json).await;
                    }
                }
                return Err(OrchestratorError::Engine {
                    status: None,
                    code: "ERR_PROVIDER_KEY".to_string(),
                    message: format!(
                        "no usable key for connection '{}': {err}; add a key or choose Local",
                        connection.name
                    ),
                });
            }

            if assessment.out_of_quota > 0 || assessment.earliest_retry_wait_secs.is_some() {
                let prior_attempt = read_prior_attempt(ctx).await;
                let attempt = prior_attempt + 1;

                if attempt > MAX_QUOTA_PAUSE_ATTEMPTS {
                    let detail = format!(
                        "quota cooldown exceeded {} pause attempts across {} keys for connection '{}'",
                        MAX_QUOTA_PAUSE_ATTEMPTS, assessment.total, connection.name
                    );
                    if let Some(ctx) = ctx {
                        let projection = crate::providers::registry::ProviderFailure {
                            class: "quota".to_string(),
                            retryable: true,
                            retry_after_secs: assessment.earliest_retry_wait_secs,
                            detail: detail.clone(),
                            status: Some(429),
                        };
                        if let Ok(json) = serde_json::to_value(&projection) {
                            let _ = ctx.merge_report_value("provider_failure", json).await;
                        }
                    }
                    return Err(OrchestratorError::Engine {
                        status: None,
                        code: "ERR_PROVIDER_QUOTA".to_string(),
                        message: format!(
                            "rate limit pause attempts exceeded limit ({MAX_QUOTA_PAUSE_ATTEMPTS}) for connection '{}': {err}",
                            connection.name
                        ),
                    });
                }

                let class = match assessment.earliest_retry_wait_secs {
                    Some(secs) if secs <= MAX_AUTO_RESUME_WAIT_SECS => RateLimitClass::PerMinute,
                    _ => RateLimitClass::Daily,
                };
                let retry_at_epoch_ms = match class {
                    RateLimitClass::PerMinute => assessment.earliest_retry_at,
                    RateLimitClass::Daily => None,
                };
                let segment_idx = ctx.map(|c| c.checkpoint.done_segments).unwrap_or(0);
                let last_error_message = match assessment.earliest_retry_wait_secs {
                    Some(secs) => format!(
                        "all {} configured keys in quota cooldown; earliest retry in {}s",
                        assessment.total, secs
                    ),
                    None => format!("all {} configured keys in quota cooldown", assessment.total),
                };

                let reason = PausedReason {
                    kind: "rate_limited".to_string(),
                    class,
                    retry_at_epoch_ms,
                    provider: provider_id.to_string(),
                    segment_idx,
                    last_error_message,
                    attempt,
                };
                return Ok(BuildAdapterOutcome::Paused(reason));
            }

            let detail = format!(
                "no usable key for connection '{}': {err}; add a key or choose Local",
                connection.name
            );
            if let Some(ctx) = ctx {
                let projection = crate::providers::registry::ProviderFailure {
                    class: "auth".to_string(),
                    retryable: false,
                    retry_after_secs: None,
                    detail: detail.clone(),
                    status: Some(401),
                };
                if let Ok(json) = serde_json::to_value(&projection) {
                    let _ = ctx.merge_report_value("provider_failure", json).await;
                }
            }
            return Err(OrchestratorError::Engine {
                status: None,
                code: "ERR_PROVIDER_KEY".to_string(),
                message: detail,
            });
        }
    };

    let adapter = match create_tts_adapter(connection, model, staging_dir, candidate.key) {
        Ok(a) => a,
        Err(err) => {
            if let Some(ctx) = ctx {
                let projection = crate::providers::registry::ProviderFailure {
                    class: "provider".to_string(),
                    retryable: false,
                    retry_after_secs: None,
                    detail: err.to_string(),
                    status: None,
                };
                if let Ok(json) = serde_json::to_value(&projection) {
                    let _ = ctx.merge_report_value("provider_failure", json).await;
                }
            }
            return Err(err);
        }
    };
    Ok(BuildAdapterOutcome::Ready(adapter, candidate.account))
}

fn wav_duration_ms(path: &Path) -> Result<i64, OrchestratorError> {
    let reader = hound::WavReader::open(path)
        .map_err(|err| provider_refusal(format!("provider audio is not a readable wav: {err}")))?;
    let spec = reader.spec();
    if spec.sample_rate == 0 {
        return Err(provider_refusal("provider wav claims 0 Hz".to_string()));
    }
    Ok((reader.duration() as i64 * 1000) / i64::from(spec.sample_rate))
}

#[derive(Debug)]
pub struct CloudSegmentOutcome {
    pub audio_hash: String,
    pub wav_bytes: i64,
    pub duration_ms: i64,
    pub native_timing: bool,
    /// The admitted native words (for the request-identity cache, so a hit
    /// reproduces the alignment); None when timing fell back.
    pub admitted_words: Option<Vec<serde_json::Value>>,
}

/// Synthesize one segment over the adapter and persist it exactly like a
/// local result: content-hashed wav in the cache (atomic rename from the
/// adapter's staging file), ledger registration, segment audio facts, and —
/// only past the whole-set gate — a native alignment row.
pub async fn synthesize_segment(
    ctx: &StageContext,
    adapter: &mut dyn TtsSynth,
    segment: &job::ScriptSegment,
    voice: &str,
    cache_dir: &Path,
    connection_name: &str,
) -> Result<CloudSegmentOutcome, OrchestratorError> {
    let artifact = call_adapter(ctx, adapter, segment, voice, connection_name, None).await?;
    persist_segment_artifact(ctx, segment, artifact, cache_dir, "").await
}

async fn call_adapter(
    ctx: &StageContext,
    adapter: &mut dyn TtsSynth,
    segment: &job::ScriptSegment,
    voice: &str,
    connection_name: &str,
    pool_outcome: PoolOutcomeSink<'_>,
) -> Result<crate::providers::registry::TtsArtifact, OrchestratorError> {
    match adapter.synthesize(&segment.text, voice).await {
        Ok(artifact) => {
            if let Some((manager, account)) = pool_outcome {
                manager.record_pool_outcome(account, Ok(()));
            }
            Ok(artifact)
        }
        Err(error) => {
            // S-5 blocker 5: auth/quota refusals demote the slot so the
            // next dial rotates past it (retry-after honored).
            if let Some((manager, account)) = pool_outcome {
                manager.record_pool_outcome(account, Err(&error));
            }
            let projection = crate::providers::registry::ProviderFailure::from_script_error(&error);
            if let Ok(json) = serde_json::to_value(&projection) {
                if let Err(report_error) = ctx.merge_report_value("provider_failure", json).await {
                    tracing::warn!("provider failure projection not recorded: {report_error}");
                }
            }
            let friendly_msg =
                friendly_provider_error(connection_name, error.status, &error.message);
            Err(OrchestratorError::Engine {
                status: None,
                code: script_error_code(error.kind).to_string(),
                message: format!(
                    "cloud tts over connection '{connection_name}' failed on segment {}: {}; \
                     the binding is preserved — retry, fix the connection, or choose Local \
                     explicitly",
                    segment.idx, friendly_msg
                ),
            })
        }
    }
}

/// Persistence runs to completion once entered — it is never raced against
/// cancellation, so a segment can only ever be fully persisted (audio hash,
/// cache ledger row, and timing verdict together) or not started. Racing it
/// would let a cancel land between `set_segment_audio` and the ledger
/// insert, leaving a resume-skipped segment whose unreferenced wav the cap
/// enforcement later evicts.
async fn persist_segment_artifact(
    ctx: &StageContext,
    segment: &job::ScriptSegment,
    artifact: crate::providers::registry::TtsArtifact,
    cache_dir: &Path,
    request_key: &str,
) -> Result<CloudSegmentOutcome, OrchestratorError> {
    // Validate WHILE STAGED (Hermes S-3 MF9): structure and duration are
    // proven on the guarded staging file, and an invalid artifact is removed
    // without ever touching its would-be published name — the
    // content-addressed cache never holds bytes that failed validation.
    let duration_ms = match wav_duration_ms(&artifact.audio_path) {
        Ok(duration_ms) if duration_ms > 0 => duration_ms,
        Ok(_) => {
            let _ = std::fs::remove_file(&artifact.audio_path);
            return Err(provider_refusal(
                "provider audio decodes to zero duration; not published".to_string(),
            ));
        }
        Err(error) => {
            let _ = std::fs::remove_file(&artifact.audio_path);
            return Err(error);
        }
    };
    let bytes = std::fs::read(&artifact.audio_path)
        .map_err(|err| provider_refusal(format!("staged audio unreadable: {err}")))?;
    let audio_hash = format!("{:x}", sha2::Sha256::digest(&bytes));
    let final_path = cache_wav_path(cache_dir, &audio_hash);
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| provider_refusal(format!("cache shard dir: {err}")))?;
    }
    if final_path.exists() {
        let _ = std::fs::remove_file(&artifact.audio_path);
    } else {
        std::fs::rename(&artifact.audio_path, &final_path)
            .map_err(|err| provider_refusal(format!("cache materialization: {err}")))?;
    }
    // Timing verdict FIRST (still nothing persisted), then ONE transaction
    // settles segment facts, cache ledger + job ref, alignment, and the
    // request-identity mapping together (Hermes R2 fold-in 6): a paid
    // response is either fully settled — mapping included — or not at all.
    let mut admitted_words: Option<Vec<serde_json::Value>> = None;
    if let Some(serde_json::Value::Array(word_values)) = artifact.native_words {
        match validate_native_words(&word_values, &segment.text, duration_ms) {
            Ok(()) => {
                admitted_words = Some(word_values);
            }
            Err(rejection) => {
                // A21: the WHOLE set falls back to local align; no partial
                // admission, and the rejection is diagnosable from the log.
                tracing::warn!(
                    job = %ctx.job_id,
                    segment = segment.idx,
                    "native timing rejected, falling back to local align: {}",
                    rejection.0
                );
            }
        }
    }
    let words_json = admitted_words
        .as_ref()
        .map(|words| WordsJson::declare(words, Timebase::Trimmed))
        .transpose()?;
    job::settle_cloud_segment(
        &ctx.pool,
        &ctx.job_id,
        &segment.id,
        &audio_hash,
        bytes.len() as i64,
        duration_ms,
        words_json.as_ref().map(|w| w.as_str()),
        request_key,
    )
    .await?;
    Ok(CloudSegmentOutcome {
        audio_hash,
        wav_bytes: bytes.len() as i64,
        duration_ms,
        native_timing: admitted_words.is_some(),
        admitted_words,
    })
}

/// Answer a segment from the request-identity cache when its audio is still
/// present: audio facts, ledger use, and (when recorded) the admitted
/// native alignment are replayed exactly. Returns None on miss or when the
/// wav was evicted (then the provider is dialed normally).
pub async fn replay_cached_request(
    ctx: &StageContext,
    request_key: &str,
    segment: &job::ScriptSegment,
    cache_dir: &Path,
) -> Result<Option<CloudSegmentOutcome>, OrchestratorError> {
    let Some(row) = job::get_provider_request_cache(&ctx.pool, request_key).await? else {
        return Ok(None);
    };
    // A hit is VALIDATED before replay (Hermes R2 fold-in 6): the bytes
    // must hash back to the row's key, decode as a WAV of the recorded
    // duration, and any recorded native timing must still pass the §3.5
    // gate. A corrupted or swapped file quarantines the mapping and reads
    // as a miss — never as somebody else's audio.
    let wav = cache_wav_path(cache_dir, &row.audio_hash);
    let Ok(bytes) = std::fs::read(&wav) else {
        // A mapping whose wav is gone is dead: quarantine it so it never
        // shadows a fresh synthesis (blocker 6).
        tracing::warn!(
            job = %ctx.job_id,
            "request-cache mapping's wav is missing; mapping quarantined"
        );
        job::delete_provider_request_cache(&ctx.pool, request_key).await?;
        return Ok(None);
    };
    let rehash = {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(&bytes))
    };
    let decoded_ms = wav_duration_ms(&wav).ok();
    // Stored timing is the versioned WordsJson ENVELOPE settlement wrote —
    // decode it as such (blocker 6: parsing it as a bare array silently
    // degraded EVERY timing-bearing hit to timestamp-less replay).
    // NULL means honestly-no-timing; a non-NULL value that does not decode
    // to versioned Trimmed words is corruption and quarantines.
    let words: Option<Vec<serde_json::Value>> = match row.words_json.as_deref() {
        None => None,
        Some(raw) => {
            let payload = WordsJson::from_stored(raw.to_string()).parse();
            if !payload.versioned
                || payload.timebase != Timebase::Trimmed
                || payload.words.is_empty()
            {
                tracing::warn!(
                    job = %ctx.job_id,
                    segment = segment.idx,
                    "request-cache mapping carries undecodable native timing; quarantined"
                );
                job::delete_provider_request_cache(&ctx.pool, request_key).await?;
                return Ok(None);
            }
            Some(payload.words)
        }
    };
    let timing_valid = match &words {
        None => true,
        Some(words) => validate_native_words(words, &segment.text, row.duration_ms).is_ok(),
    };
    if rehash != row.audio_hash
        || bytes.len() as i64 != row.wav_bytes
        || decoded_ms != Some(row.duration_ms)
        || !timing_valid
    {
        tracing::warn!(
            job = %ctx.job_id,
            segment = segment.idx,
            "request-cache mapping failed validation; quarantined and treated as a miss"
        );
        job::delete_provider_request_cache(&ctx.pool, request_key).await?;
        return Ok(None);
    }
    let words_json = words
        .as_ref()
        .map(|words| WordsJson::declare(words, Timebase::Trimmed))
        .transpose()?;
    job::settle_cloud_segment(
        &ctx.pool,
        &ctx.job_id,
        &segment.id,
        &row.audio_hash,
        row.wav_bytes,
        row.duration_ms,
        words_json.as_ref().map(|w| w.as_str()),
        request_key,
    )
    .await?;
    tracing::info!(
        job = %ctx.job_id,
        segment = segment.idx,
        "request-identity cache hit: no provider request issued"
    );
    Ok(Some(CloudSegmentOutcome {
        audio_hash: row.audio_hash,
        wav_bytes: row.wav_bytes,
        duration_ms: row.duration_ms,
        native_timing: words.is_some(),
        admitted_words: None,
    }))
}

#[derive(Debug)]
pub enum SegmentRun {
    Done(CloudSegmentOutcome),
    Interrupted(StageOutcome),
    RateLimited(PausedReason),
}

/// One segment's synthesis with PROMPT in-flight cancellation: the control
/// signal races ONLY the provider request, and winning the race DROPS the
/// request future — reqwest aborts the in-flight HTTP exchange on drop, so
/// a cancel lands mid-request, not at the next segment boundary. A dropped
/// request persists nothing (the adapter stages its artifact only after the
/// wire completes). Once the wire HAS completed, persistence runs to
/// completion un-raced: a segment is fully persisted or not started, never
/// half (see `persist_segment_artifact`).
///
/// Multi-Key Pool Rotation & Quota Fault Tolerance:
/// When a request fails with Quota (429) or Auth (revoked key):
/// 1. Demotes the active key slot in `KeyManager` so rotation moves past it.
/// 2. If another key is available in the pool, rotates to the next candidate
///    and immediately retries synthesis for the segment.
/// 3. If all keys in the pool hit quota cooldown, waits for the earliest unelapsed
///    retry deadline (up to `MAX_QUOTA_WAIT_SECS`, default 20s) with cancellation
///    awareness, then retries up to `MAX_QUOTA_BACKOFF_RETRIES` times.
#[allow(clippy::too_many_arguments)]
pub async fn synthesize_segment_cancellable(
    ctx: &StageContext,
    plan: &mut CloudTtsPlan,
    segment: &job::ScriptSegment,
    voice: &str,
    cache_dir: &Path,
    request_key: &str,
    keys: Option<&KeyManager>,
) -> Result<SegmentRun, OrchestratorError> {
    let mut watcher = ctx.control.clone();
    let mut quota_retries: u32 = 0;
    let mut retryable_retries: u32 = 0;
    let mut overload_attempts: usize = 0;
    let mut overload_start: Option<tokio::time::Instant> = None;
    let patience = plan.patience_config.clone().unwrap_or_default();
    let is_overnight = job::get_job(&ctx.pool, &ctx.job_id)
        .await
        .ok()
        .flatten()
        .map(|j| j.overnight)
        .unwrap_or(false);
    let budget = if patience.backoff_delays_secs.iter().all(|&d| d == 0) {
        PatienceBudget::for_test()
    } else {
        PatienceBudget::for_mode(is_overnight)
    };

    loop {
        if let Some(outcome) = watcher.interruption() {
            return Ok(SegmentRun::Interrupted(outcome));
        }

        let synth_res = tokio::select! {
            res = plan.adapter.synthesize(&segment.text, voice) => res,
            outcome = watcher.interrupted() => {
                plan.adapter.cancel().await;
                return Ok(SegmentRun::Interrupted(outcome));
            },
        };

        match synth_res {
            Ok(artifact) => {
                if let Some(manager) = keys {
                    manager.record_pool_outcome(&plan.account, Ok(()));
                }
                return persist_segment_artifact(ctx, segment, artifact, cache_dir, request_key)
                    .await
                    .map(SegmentRun::Done);
            }
            Err(error) => {
                let fault = ProviderErrorPolicy::classify_script_error(&error);
                match fault {
                    FaultClass::ClusterOverload { .. } | FaultClass::TransientTransport => {
                        let retry_after = match fault {
                            FaultClass::ClusterOverload { retry_after_secs } => retry_after_secs,
                            _ => None,
                        };
                        if overload_start.is_none() {
                            overload_start = Some(tokio::time::Instant::now());
                        }
                        let elapsed = overload_start.unwrap().elapsed();
                        if overload_attempts < budget.max_cluster_attempts
                            && elapsed.as_secs() < patience.max_window_secs
                        {
                            overload_attempts += 1;
                            let delay = budget.calculate_delay(overload_attempts, retry_after);
                            let delay_secs = delay.as_secs_f64();
                            tracing::warn!(
                                segment = segment.idx,
                                provider = %plan.connection.name,
                                attempt = overload_attempts,
                                max_attempts = budget.max_cluster_attempts,
                                delay_secs,
                                is_overnight = budget.is_overnight,
                                "cloud tts provider overloaded / transient transport; retrying on same key with jittered backoff"
                            );
                            tokio::select! {
                                outcome = watcher.interrupted() => return Ok(SegmentRun::Interrupted(outcome)),
                                _ = tokio::time::sleep(delay) => {}
                            }
                            continue;
                        }

                        let attempt = ctx
                            .checkpoint
                            .paused_reason
                            .as_ref()
                            .map(|r| r.attempt)
                            .unwrap_or(0)
                            + 1;
                        if attempt > MAX_OVERLOAD_PAUSE_ATTEMPTS {
                            return Err(provider_refusal(format!(
                                "provider '{}' remained overloaded across {MAX_OVERLOAD_PAUSE_ATTEMPTS} pause attempts; pausing exhausted",
                                plan.connection.name
                            )));
                        }
                        let retry_at_epoch_ms =
                            Some(now_ms() + (patience.pause_wait_secs as i64 * 1000));
                        let friendly_msg = friendly_provider_error(
                            &plan.connection.name,
                            error.status,
                            &error.message,
                        );
                        let reason = PausedReason {
                            kind: PAUSED_KIND_SERVICE_BUSY.to_string(),
                            class: RateLimitClass::PerMinute,
                            retry_at_epoch_ms,
                            provider: plan.connection.provider.vault_provider().id().to_string(),
                            segment_idx: segment.idx,
                            last_error_message: friendly_msg,
                            attempt,
                        };
                        tracing::warn!(
                            segment = segment.idx,
                            provider = %plan.connection.name,
                            retry_at_epoch_ms = ?retry_at_epoch_ms,
                            attempt,
                            "cloud tts provider overloaded past patience window; pausing job honestly"
                        );
                        return Ok(SegmentRun::RateLimited(reason));
                    }
                    FaultClass::KeyAuthInvalid => {
                        if let Some(manager) = keys {
                            manager.record_dial_auth_failure(&plan.account, None);
                            manager.advance_pool_cursor(&plan.account);
                            let provider_id = plan.connection.provider.vault_provider().id();
                            let base_url = plan.connection.key_base_url();

                            if let Ok(candidate) = manager
                                .resolve_pool_key(&ctx.pool, provider_id, &base_url)
                                .await
                            {
                                tracing::info!(
                                    old_account = %plan.account,
                                    new_account = %candidate.account,
                                    segment = segment.idx,
                                    "cloud tts rotated key on segment {} after auth invalid; continuing without sleep",
                                    segment.idx
                                );
                                plan.replace_adapter(candidate.key, candidate.account)?;
                                continue;
                            }
                        }

                        let friendly_msg = friendly_provider_error(
                            &plan.connection.name,
                            error.status,
                            &error.message,
                        );
                        return Err(OrchestratorError::Engine {
                            status: error.status,
                            code: "ERR_PROVIDER_AUTH".to_string(),
                            message: format!(
                                "cloud tts over connection '{}' failed with auth error on segment {}: {}",
                                plan.connection.name, segment.idx, friendly_msg
                            ),
                        });
                    }
                    FaultClass::KeyRateLimited { retry_after_secs } => {
                        if let Some(manager) = keys {
                            manager.record_dial_quota(&plan.account, None, retry_after_secs);
                            manager.advance_pool_cursor(&plan.account);
                            let provider_id = plan.connection.provider.vault_provider().id();
                            let base_url = plan.connection.key_base_url();

                            match manager
                                .resolve_pool_key(&ctx.pool, provider_id, &base_url)
                                .await
                            {
                                Ok(candidate) => {
                                    tracing::info!(
                                        old_account = %plan.account,
                                        new_account = %candidate.account,
                                        segment = segment.idx,
                                        "cloud tts rotated key on segment {} after 429 rate limit; continuing without sleep",
                                        segment.idx
                                    );
                                    plan.replace_adapter(candidate.key, candidate.account)?;
                                    continue;
                                }
                                Err(key_err) => {
                                    tracing::warn!(
                                        segment = segment.idx,
                                        error = %key_err,
                                        "no alternative key immediately available in pool after rate limit"
                                    );

                                    if classify_quota_error(&error) == RateLimitClass::PerMinute
                                        && !retry_after_secs
                                            .is_some_and(|s| s > MAX_QUOTA_WAIT_SECS)
                                    {
                                        let mut resolved = false;
                                        while quota_retries < MAX_QUOTA_BACKOFF_RETRIES {
                                            quota_retries += 1;
                                            let earliest_pool_wait = manager
                                                .earliest_quota_retry_wait_secs(
                                                    &ctx.pool,
                                                    Some(provider_id),
                                                    &base_url,
                                                )
                                                .await
                                                .ok()
                                                .flatten();
                                            let wait_secs = retry_after_secs
                                                .or(earliest_pool_wait)
                                                .unwrap_or(DEFAULT_QUOTA_BACKOFF_SECS)
                                                .min(MAX_QUOTA_WAIT_SECS);

                                            tracing::warn!(
                                                segment = segment.idx,
                                                wait_secs,
                                                quota_retries,
                                                max_retries = MAX_QUOTA_BACKOFF_RETRIES,
                                                "all keys in pool rate-limited; waiting for quota cooldown before retry"
                                            );
                                            tokio::select! {
                                                outcome = watcher.interrupted() => return Ok(SegmentRun::Interrupted(outcome)),
                                                _ = tokio::time::sleep(std::time::Duration::from_secs(wait_secs)) => {}
                                            }

                                            if let Ok(candidate) = manager
                                                .resolve_pool_key(&ctx.pool, provider_id, &base_url)
                                                .await
                                            {
                                                tracing::info!(
                                                    old_account = %plan.account,
                                                    new_account = %candidate.account,
                                                    segment = segment.idx,
                                                    "quota cooldown elapsed; resolved key from pool, retrying"
                                                );
                                                plan.replace_adapter(
                                                    candidate.key,
                                                    candidate.account,
                                                )?;
                                                resolved = true;
                                                break;
                                            }
                                        }
                                        if resolved {
                                            continue;
                                        }
                                    }
                                }
                            }
                        }

                        let class = classify_quota_error(&error);
                        let retry_at_epoch_ms = match class {
                            RateLimitClass::PerMinute => {
                                let wait_secs = retry_after_secs
                                    .unwrap_or(DEFAULT_AUTO_RESUME_WAIT_SECS)
                                    .clamp(MIN_AUTO_RESUME_WAIT_SECS, MAX_AUTO_RESUME_WAIT_SECS);
                                Some(now_ms() + (wait_secs as i64 * 1000))
                            }
                            RateLimitClass::Daily => None,
                        };
                        let attempt = ctx
                            .checkpoint
                            .paused_reason
                            .as_ref()
                            .map(|r| r.attempt)
                            .unwrap_or(0)
                            + 1;
                        if attempt > MAX_QUOTA_PAUSE_ATTEMPTS {
                            let projection =
                                crate::providers::registry::ProviderFailure::from_script_error(
                                    &error,
                                );
                            if let Ok(json) = serde_json::to_value(&projection) {
                                let _ = ctx.merge_report_value("provider_failure", json).await;
                            }
                            let friendly_msg = friendly_provider_error(
                                &plan.connection.name,
                                error.status,
                                &error.message,
                            );
                            return Err(OrchestratorError::Engine {
                                status: None,
                                code: "ERR_PROVIDER_QUOTA".to_string(),
                                message: format!(
                                    "cloud tts over connection '{}' hit quota pause limit ({} attempts) on segment {}: {}",
                                    plan.connection.name, MAX_QUOTA_PAUSE_ATTEMPTS, segment.idx, friendly_msg
                                ),
                            });
                        }
                        let reason = PausedReason {
                            kind: "rate_limited".to_string(),
                            class,
                            retry_at_epoch_ms,
                            provider: plan.connection.provider.vault_provider().id().to_string(),
                            segment_idx: segment.idx,
                            last_error_message: sanitize_error_message(&error.message),
                            attempt,
                        };
                        tracing::warn!(
                            segment = segment.idx,
                            class = ?class,
                            retry_at_epoch_ms = ?retry_at_epoch_ms,
                            attempt,
                            "cloud tts rate-limited; pausing job honestly"
                        );
                        return Ok(SegmentRun::RateLimited(reason));
                    }
                    FaultClass::Terminal => {
                        if error.retryable && retryable_retries < MAX_RETRYABLE_RETRIES {
                            retryable_retries += 1;
                            if let Some(manager) = keys {
                                manager.advance_pool_cursor(&plan.account);
                                let provider_id = plan.connection.provider.vault_provider().id();
                                let base_url = plan.connection.key_base_url();
                                if let Ok(candidate) = manager
                                    .resolve_pool_key(&ctx.pool, provider_id, &base_url)
                                    .await
                                {
                                    if candidate.account != plan.account {
                                        tracing::info!(
                                            old_account = %plan.account,
                                            new_account = %candidate.account,
                                            segment = segment.idx,
                                            attempt = retryable_retries,
                                            "cloud tts rotated key on segment {} after retryable error: {}",
                                            segment.idx,
                                            error.message
                                        );
                                        plan.replace_adapter(candidate.key, candidate.account)?;
                                        continue;
                                    }
                                }
                            }
                            tracing::warn!(
                                segment = segment.idx,
                                attempt = retryable_retries,
                                "retrying segment {} after retryable error: {}",
                                segment.idx,
                                error.message
                            );
                            tokio::select! {
                                outcome = watcher.interrupted() => return Ok(SegmentRun::Interrupted(outcome)),
                                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
                            }
                            continue;
                        }

                        if let Some(manager) = keys {
                            manager.record_pool_outcome(&plan.account, Err(&error));
                        }
                        let projection =
                            crate::providers::registry::ProviderFailure::from_script_error(&error);
                        if let Ok(json) = serde_json::to_value(&projection) {
                            let _ = ctx.merge_report_value("provider_failure", json).await;
                        }
                        let friendly_msg = friendly_provider_error(
                            &plan.connection.name,
                            error.status,
                            &error.message,
                        );
                        return Err(OrchestratorError::Engine {
                            status: error.status,
                            code: script_error_code(error.kind).to_string(),
                            message: format!(
                                "cloud tts over connection '{}' failed on segment {}: {}",
                                plan.connection.name, segment.idx, friendly_msg
                            ),
                        });
                    }
                }
            }
        }
    }
}

/// The semantic request-cache identity (Hermes MF6): provider + canonical
/// endpoint + model + voice + exact text + adapter revision. Anything less
/// (the output-content hash alone) cannot prevent paid re-synthesis.
pub fn request_cache_key(
    provider: &str,
    endpoint: &str,
    model: &str,
    voice: &str,
    text: &str,
    adapter_rev: &str,
) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    // Length-prefixed fields (Hermes R2 blocker 6): tuple boundaries are
    // formally unambiguous whatever bytes the text contains.
    for part in [provider, endpoint, model, voice, text, adapter_rev] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub type TestAdapterFactory = Arc<dyn Fn(&ApiKey, &str) -> Box<dyn TtsSynth> + Send + Sync>;

pub struct CloudTtsPlan {
    pub connection: ConnectionRecord,
    pub adapter: Box<dyn TtsSynth>,
    /// The custody account the stage's key came from — the outcome-feedback
    /// address for `record_pool_outcome` (S-5 blocker 5).
    pub account: String,
    pub model: String,
    pub staging_dir: std::path::PathBuf,
    #[doc(hidden)]
    pub test_adapter_factory: Option<TestAdapterFactory>,
    pub patience_config: Option<PatienceConfig>,
}

impl CloudTtsPlan {
    pub fn for_testing(
        connection: ConnectionRecord,
        adapter: Box<dyn TtsSynth>,
        account: String,
        model: String,
        staging_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            connection,
            adapter,
            account,
            model,
            staging_dir,
            test_adapter_factory: None,
            patience_config: None,
        }
    }

    pub fn replace_adapter(
        &mut self,
        key: ApiKey,
        account: String,
    ) -> Result<(), OrchestratorError> {
        self.account = account.clone();
        if let Some(factory) = &self.test_adapter_factory {
            self.adapter = factory(&key, &account);
            return Ok(());
        }
        self.adapter =
            create_tts_adapter(&self.connection, &self.model, self.staging_dir.clone(), key)?;
        Ok(())
    }

    /// Rotates `plan.adapter` to the next available pool key for round-robin distribution.
    /// Returns `Ok(true)` if the adapter was rotated to a different account, or `Ok(false)` if no rotation occurred.
    pub async fn rotate_pool(
        &mut self,
        pool: &sqlx::SqlitePool,
        keys: Option<&KeyManager>,
    ) -> Result<bool, OrchestratorError> {
        let Some(manager) = keys else {
            return Ok(false);
        };
        let provider_id = self.connection.provider.vault_provider().id();
        let base_url = self.connection.key_base_url();
        if let Ok(next) = manager.resolve_pool_key(pool, provider_id, &base_url).await {
            if next.account != self.account {
                self.replace_adapter(next.key, next.account)?;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

pub enum PreparedCloudTts {
    Ready(CloudTtsPlan),
    Paused(PausedReason),
}

impl PreparedCloudTts {
    pub fn unwrap_plan(self) -> CloudTtsPlan {
        match self {
            PreparedCloudTts::Ready(plan) => plan,
            PreparedCloudTts::Paused(reason) => {
                panic!("expected PreparedCloudTts::Ready, got Paused: {:?}", reason);
            }
        }
    }
}

impl std::fmt::Debug for PreparedCloudTts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(plan) => f
                .debug_struct("Ready")
                .field("connection", &plan.connection.id)
                .field("account", &plan.account)
                .field("model", &plan.model)
                .finish(),
            Self::Paused(reason) => f.debug_tuple("Paused").field(reason).finish(),
        }
    }
}

/// Vet the recorded binding and build the adapter (enabled check,
/// servable-phase check, key custody). The stage owns the segment loop.
pub async fn prepare_cloud_tts(
    pool: &sqlx::SqlitePool,
    keys: Option<&Arc<KeyManager>>,
    connection: ConnectionRecord,
    model: &str,
    cache_dir: &Path,
    ctx: Option<&StageContext>,
) -> Result<PreparedCloudTts, OrchestratorError> {
    let staging_dir = cache_dir.join("provider_staging");
    match build_tts_adapter(pool, keys, &connection, model, staging_dir.clone(), ctx).await? {
        BuildAdapterOutcome::Ready(adapter, account) => Ok(PreparedCloudTts::Ready(CloudTtsPlan {
            connection,
            adapter,
            account,
            model: model.to_string(),
            staging_dir,
            test_adapter_factory: None,
            patience_config: None,
        })),
        BuildAdapterOutcome::Paused(reason) => Ok(PreparedCloudTts::Paused(reason)),
    }
}

/// A22: a timestamp-less provider plus no installed align model means
/// read-along timing will honestly degrade; the report says so before the
/// listener asks.
pub fn align_advisory_needed(provider: ProviderKind, align_model_resolved: bool) -> bool {
    matches!(provider, ProviderKind::OpenaiTts) && !align_model_resolved
}

/// Interruption checkpoint between segments; the outcome is the control's
/// (pause vs cancel), never assumed.
pub fn interruption(ctx: &StageContext) -> Option<StageOutcome> {
    ctx.control.interruption()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::registry::ScriptErrorKind;

    #[test]
    fn depleted_credits_never_wait_as_a_per_minute_limit() {
        let error = ScriptError {
            kind: ScriptErrorKind::Quota,
            retryable: true,
            message: "provider returned HTTP 402 — Your prepayment credits are depleted".into(),
            retry_after_secs: None,
            status: Some(402),
        };
        assert_eq!(classify_quota_error(&error), RateLimitClass::Daily);
    }

    #[test]
    fn absent_or_unrelated_overrides_carry_no_tts_binding() {
        assert_eq!(override_tts_binding(None).unwrap(), None);
        assert_eq!(
            override_tts_binding(Some(r#"{"llm":"phi-4-mini"}"#)).unwrap(),
            None
        );
        assert_eq!(override_tts_binding(Some("not json")).unwrap(), None);
    }

    #[test]
    fn a_malformed_tts_binding_refuses_instead_of_going_local() {
        for malformed in [
            r#"{"tts_binding":"kokoro-82m"}"#,
            r#"{"tts_binding":{"kind":"connection","connection_id":"c","model":"m"}}"#,
            r#"{"tts_binding":{"kind":"connection","connection_id":"c","model":"m","voice":"v"}}"#,
        ] {
            assert!(
                override_tts_binding(Some(malformed)).is_err(),
                "{malformed} must refuse"
            );
        }
    }

    #[test]
    fn a_structural_tts_binding_parses() {
        let binding = override_tts_binding(Some(
            r#"{"tts_binding":{"kind":"connection","connection_id":"c-1","model":"sonic-3.6","voice":"v-9","provider":"cartesia_tts","endpoint":"https://api.cartesia.ai","adapter_rev":"cartesia_tts/1"}}"#,
        ))
        .unwrap();
        assert_eq!(
            binding,
            Some(TtsBinding::Connection {
                connection_id: "c-1".to_string(),
                model: "sonic-3.6".to_string(),
                voice: "v-9".to_string(),
                provider: "cartesia_tts".to_string(),
                endpoint: "https://api.cartesia.ai".to_string(),
                adapter_rev: "cartesia_tts/1".to_string(),
            })
        );
    }

    #[test]
    fn the_align_advisory_names_exactly_the_timestampless_gap() {
        assert!(align_advisory_needed(ProviderKind::OpenaiTts, false));
        assert!(!align_advisory_needed(ProviderKind::OpenaiTts, true));
        assert!(!align_advisory_needed(ProviderKind::CartesiaTts, false));
    }

    use crate::orchestrator::stage::{control_channel, ProgressEvent};
    use crate::orchestrator::state::{Checkpoint, JobState};
    use crate::providers::registry::{ScriptError, TtsArtifact};
    use crate::storage::{job, new_id, now_ms};
    use crate::voice::keys::store::Backend;
    use crate::voice::keys::{KeyManager, KeyState, KeyStore};
    use async_trait::async_trait;
    use sqlx::SqlitePool;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn write_test_wav(path: &Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 24_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(path, spec).unwrap();
        for sample in 0..2400i16 {
            wav.write_sample(sample).unwrap();
        }
        wav.finalize().unwrap();
    }

    struct MockSynth {
        key: String,
        call_count: Arc<AtomicU32>,
        staging_dir: std::path::PathBuf,
        fail_first_quota: bool,
        max_quota_failures: u32,
        fail_auth: bool,
        fail_retryable_provider: bool,
        retry_after: Option<u64>,
    }

    #[async_trait]
    impl TtsSynth for MockSynth {
        async fn synthesize(
            &mut self,
            _text: &str,
            _voice: &str,
        ) -> Result<TtsArtifact, ScriptError> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if self.fail_retryable_provider {
                return Err(ScriptError {
                    kind: ScriptErrorKind::Provider,
                    retryable: true,
                    message: "Gemini returned no inline audio data in candidate parts".to_string(),
                    retry_after_secs: None,
                    status: None,
                });
            }
            if self.fail_first_quota
                && self.key.contains("slot-0")
                && count < self.max_quota_failures
            {
                let message = if self.key.contains("daily") {
                    "Resource has been exhausted: requests per day (RPD) limit exceeded".to_string()
                } else if self.key.contains("leak-key") {
                    "Quota exceeded for key AIzaSyA1234567890123456789012345678901 (HTTP 429)"
                        .to_string()
                } else {
                    "Quota exceeded (HTTP 429)".to_string()
                };
                return Err(ScriptError {
                    kind: ScriptErrorKind::Quota,
                    retryable: true,
                    message,
                    retry_after_secs: self.retry_after,
                    status: Some(429),
                });
            }
            if self.fail_auth && self.key.contains("slot-0") {
                return Err(ScriptError {
                    kind: ScriptErrorKind::Auth,
                    retryable: false,
                    message: "Unauthorized (HTTP 401)".to_string(),
                    retry_after_secs: None,
                    status: Some(401),
                });
            }
            std::fs::create_dir_all(&self.staging_dir).unwrap();
            let audio_path = self
                .staging_dir
                .join(format!("test_{}_{}.wav", self.key, count));
            write_test_wav(&audio_path);
            Ok(TtsArtifact {
                audio_path,
                native_words: None,
            })
        }
    }

    async fn setup_test_harness(
        num_keys: usize,
    ) -> (
        tempfile::TempDir,
        SqlitePool,
        Arc<KeyManager>,
        ConnectionRecord,
        StageContext,
        crate::orchestrator::stage::ControlHandle,
        job::ScriptSegment,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();
        let store = KeyStore::with_forced_backend(
            dir.path().join("keys.vault"),
            Backend::Session(Default::default()),
        );
        let manager = Arc::new(KeyManager::with_store(store));
        for i in 0..num_keys {
            manager
                .add_key(
                    &pool,
                    "gemini",
                    None,
                    format!("key-material-for-slot-{i}-000"),
                    Some(&format!("KEY_{i}")),
                )
                .await
                .unwrap();
        }

        let book_record = crate::storage::book::Book {
            id: "book-1".into(),
            title: "Test".into(),
            author: None,
            source_path: "/tmp/book.epub".into(),
            format: "epub".into(),
            page_count: Some(10),
            language: Some("en".into()),
            is_scanned: false,
            ingest_path: Some("fast".into()),
            cover_path: None,
            content_hash: "hash".into(),
            blocks_version: None,
            blocks_page_count: None,
            layout_analysed: false,
            reflowed: false,
            extracting_since: None,
            created_at: now_ms(),
        };
        crate::storage::book::insert_book(&pool, &book_record)
            .await
            .unwrap();

        let chapter = crate::storage::book::Chapter {
            id: "chap-0".into(),
            book_id: "book-1".into(),
            idx: 0,
            title: "Chapter 0".into(),
            page_start: 1,
            page_end: 10,
            is_pseudo: false,
            word_count: Some(100),
        };
        crate::storage::book::insert_chapter(&pool, &chapter)
            .await
            .unwrap();

        let profile = crate::storage::voice::VoiceProfile {
            id: "voice-1".into(),
            name: "Default".into(),
            engine: "gemini".into(),
            kind: "preset".into(),
            preset_voice_id: Some("Puck".into()),
            reference_wav_path: None,
            params_json: None,
            created_at: now_ms(),
        };
        crate::storage::voice::insert_voice_profile(&pool, &profile)
            .await
            .unwrap();

        let job_id = new_id();
        let render_job = job::RenderJob {
            id: job_id.clone(),
            book_id: "book-1".into(),
            scope_json: "{}".into(),
            mode: "narrator".into(),
            voice_profile_id: "voice-1".into(),
            output_format: "mp3".into(),
            model_overrides_json: None,
            resource_plan_json: None,
            vision_plan_json: None,
            tts_plan_json: None,
            align_plan_json: None,
            state: JobState::Synthesizing.as_str().into(),
            checkpoint_json: None,
            overnight: false,
            eta_seconds: None,
            report_json: None,
            output_paths_json: None,
            tts_seed: None,
            verify: false,
            created_at: now_ms(),
            updated_at: now_ms(),
        };
        job::insert_job(&pool, &render_job).await.unwrap();

        let segment = job::ScriptSegment {
            id: new_id(),
            job_id: job_id.clone(),
            chapter_id: "chap-0".into(),
            idx: 0,
            page_no: 1,
            kind: "text".into(),
            figure_id: None,
            text: "Hello world.".into(),
            source_span_json: None,
            flags_json: None,
            audio_hash: None,
            duration_ms: None,
            original_text: None,
        };
        job::insert_segments(&pool, std::slice::from_ref(&segment))
            .await
            .unwrap();

        let (control_handle, control_rx) = control_channel();
        let progress_sink = Arc::new(|_: ProgressEvent| {});
        let ctx = StageContext::new(
            pool.clone(),
            job_id,
            JobState::Synthesizing,
            Checkpoint::new(JobState::Synthesizing, now_ms()),
            control_rx,
            progress_sink,
        );

        let connection = ConnectionRecord::draft("gemini", None, false).unwrap();
        (dir, pool, manager, connection, ctx, control_handle, segment)
    }

    #[tokio::test]
    async fn quota_429_rotates_to_next_pool_key_and_succeeds() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(2).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        assert_eq!(plan.account, "https://generativelanguage.googleapis.com");

        let call_count = Arc::new(AtomicU32::new(0));
        let count_clone = call_count.clone();
        let staging_dir = plan.staging_dir.clone();

        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockSynth {
                key: key.expose().to_string(),
                call_count: count_clone.clone(),
                staging_dir: staging_dir.clone(),
                fail_first_quota: true,
                max_quota_failures: 1,
                fail_auth: false,
                fail_retryable_provider: false,
                retry_after: Some(30),
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-000".into()),
            &plan.account,
        );

        let run = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-1",
            Some(manager.as_ref()),
        )
        .await
        .unwrap();

        match run {
            SegmentRun::Done(outcome) => {
                assert!(outcome.duration_ms > 0);
            }
            other => panic!("expected SegmentRun::Done, got {other:?}"),
        }

        // Must have rotated to slot 1!
        assert_eq!(plan.account, "https://generativelanguage.googleapis.com#1");
        assert_eq!(call_count.load(Ordering::SeqCst), 2);

        // Slot 0 demoted to OutOfQuota in KeyManager
        let keys = manager.list_entries(&pool).await.unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].slot, 0);
        assert!(matches!(keys[0].state, KeyState::OutOfQuota));
        assert_eq!(keys[1].slot, 1);
        assert_eq!(keys[1].state, KeyState::Active);
    }

    #[tokio::test]
    async fn auth_failure_rotates_to_next_pool_key_and_succeeds() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(2).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        assert_eq!(plan.account, "https://generativelanguage.googleapis.com");

        let call_count = Arc::new(AtomicU32::new(0));
        let count_clone = call_count.clone();
        let staging_dir = plan.staging_dir.clone();

        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockSynth {
                key: key.expose().to_string(),
                call_count: count_clone.clone(),
                staging_dir: staging_dir.clone(),
                fail_first_quota: false,
                max_quota_failures: 0,
                fail_auth: true,
                fail_retryable_provider: false,
                retry_after: None,
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-000".into()),
            &plan.account,
        );

        let run = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-2",
            Some(manager.as_ref()),
        )
        .await
        .unwrap();

        match run {
            SegmentRun::Done(outcome) => {
                assert!(outcome.duration_ms > 0);
            }
            other => panic!("expected SegmentRun::Done, got {other:?}"),
        }

        // Must have rotated to slot 1!
        assert_eq!(plan.account, "https://generativelanguage.googleapis.com#1");

        // Slot 0 demoted to Invalid
        let keys = manager.list_entries(&pool).await.unwrap();
        assert_eq!(keys[0].state, KeyState::Invalid);
        assert_eq!(keys[1].state, KeyState::Active);
    }

    #[tokio::test]
    async fn quota_failure_with_unreasonable_wait_fails_cleanly() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(1).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        let staging_dir = plan.staging_dir.clone();
        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockSynth {
                key: key.expose().to_string(),
                call_count: Arc::new(AtomicU32::new(0)),
                staging_dir: staging_dir.clone(),
                fail_first_quota: true,
                max_quota_failures: u32::MAX,
                fail_auth: false,
                fail_retryable_provider: false,
                retry_after: Some(3600), // 1 hour > MAX_QUOTA_WAIT_SECS (180s)
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-000".into()),
            &plan.account,
        );

        let res = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-3",
            Some(manager.as_ref()),
        )
        .await;

        match res {
            Ok(SegmentRun::RateLimited(reason)) => {
                assert_eq!(reason.class, RateLimitClass::Daily);
                assert!(reason.retry_at_epoch_ms.is_none());
                assert_eq!(reason.attempt, 1);
            }
            other => panic!("expected SegmentRun::RateLimited(Daily), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn quota_failure_per_minute_exhaustion_pauses_with_scheduled_retry() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(1).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        let staging_dir = plan.staging_dir.clone();
        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockSynth {
                key: key.expose().to_string(),
                call_count: Arc::new(AtomicU32::new(0)),
                staging_dir: staging_dir.clone(),
                fail_first_quota: true,
                max_quota_failures: u32::MAX,
                fail_auth: false,
                fail_retryable_provider: false,
                retry_after: Some(45),
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-000".into()),
            &plan.account,
        );

        let res = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-per-minute",
            None,
        )
        .await;

        match res {
            Ok(SegmentRun::RateLimited(reason)) => {
                assert_eq!(reason.class, RateLimitClass::PerMinute);
                assert!(reason.retry_at_epoch_ms.is_some());
                let retry_at = reason.retry_at_epoch_ms.unwrap();
                assert!(retry_at > now_ms());
                assert_eq!(reason.attempt, 1);
            }
            other => panic!("expected SegmentRun::RateLimited(PerMinute), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn quota_failure_daily_indicator_pauses_without_retry() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(1).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        let staging_dir = plan.staging_dir.clone();
        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockSynth {
                key: key.expose().to_string(),
                call_count: Arc::new(AtomicU32::new(0)),
                staging_dir: staging_dir.clone(),
                fail_first_quota: true,
                max_quota_failures: u32::MAX,
                fail_auth: false,
                fail_retryable_provider: false,
                retry_after: None,
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-daily".into()),
            &plan.account,
        );

        let res = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-daily",
            None,
        )
        .await;

        match res {
            Ok(SegmentRun::RateLimited(reason)) => {
                assert_eq!(reason.class, RateLimitClass::Daily);
                assert!(reason.retry_at_epoch_ms.is_none());
                assert!(reason.last_error_message.contains("requests per day"));
            }
            other => panic!("expected SegmentRun::RateLimited(Daily), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn quota_failure_sanitizes_leaked_key_in_error_message() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(1).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        let staging_dir = plan.staging_dir.clone();
        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockSynth {
                key: key.expose().to_string(),
                call_count: Arc::new(AtomicU32::new(0)),
                staging_dir: staging_dir.clone(),
                fail_first_quota: true,
                max_quota_failures: u32::MAX,
                fail_auth: false,
                fail_retryable_provider: false,
                retry_after: Some(30),
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-leak-key".into()),
            &plan.account,
        );

        let res = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-leak",
            None,
        )
        .await;

        match res {
            Ok(SegmentRun::RateLimited(reason)) => {
                assert!(!reason.last_error_message.contains("AIzaSyA"));
                assert!(reason.last_error_message.contains("[REDACTED_API_KEY]"));
            }
            other => panic!("expected SegmentRun::RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_synthesis_cooperatively() {
        let (dir, pool, manager, connection, ctx, control, segment) = setup_test_harness(1).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        control.cancel();

        let run = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-4",
            Some(manager.as_ref()),
        )
        .await
        .unwrap();

        match run {
            SegmentRun::Interrupted(crate::orchestrator::stage::StageOutcome::Cancelled) => {}
            other => panic!("expected Interrupted(Cancelled), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retryable_provider_error_fails_cleanly_without_silence_fallback() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(1).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        let call_count = Arc::new(AtomicU32::new(0));
        let count_clone = call_count.clone();
        let staging_dir = plan.staging_dir.clone();

        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockSynth {
                key: key.expose().to_string(),
                call_count: count_clone.clone(),
                staging_dir: staging_dir.clone(),
                fail_first_quota: false,
                max_quota_failures: 0,
                fail_auth: false,
                fail_retryable_provider: true,
                retry_after: None,
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-000".into()),
            &plan.account,
        );

        let res = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-retryable-fail",
            Some(manager.as_ref()),
        )
        .await;

        match res {
            Err(OrchestratorError::Engine { code, message, .. }) => {
                assert_eq!(code, "ERR_PROVIDER");
                assert!(message.contains("inline audio"));
            }
            other => panic!("expected OrchestratorError::Engine, got {other:?}"),
        }

        // Initial attempt (1) + MAX_RETRYABLE_RETRIES (2) = 3 calls
        assert_eq!(call_count.load(Ordering::SeqCst), 3);

        // Verify NO request-cache row was written for failed segment
        let cached = job::get_provider_request_cache(&pool, "req-key-retryable-fail")
            .await
            .unwrap();
        assert!(cached.is_none());
    }

    #[tokio::test]
    async fn round_robin_pool_rotation_advances_each_segment() {
        let (dir, pool, manager, connection, _ctx, _control, _segment) =
            setup_test_harness(3).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&_ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        let staging_dir = plan.staging_dir.clone();
        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockSynth {
                key: key.expose().to_string(),
                call_count: Arc::new(AtomicU32::new(0)),
                staging_dir: staging_dir.clone(),
                fail_first_quota: false,
                max_quota_failures: 0,
                fail_auth: false,
                fail_retryable_provider: false,
                retry_after: None,
            })
        });
        plan.test_adapter_factory = Some(factory.clone());

        assert_eq!(plan.account, "https://generativelanguage.googleapis.com");
        manager.record_pool_outcome(&plan.account, Ok(()));
        let rotated = plan
            .rotate_pool(&pool, Some(manager.as_ref()))
            .await
            .unwrap();
        assert!(rotated);
        assert_eq!(plan.account, "https://generativelanguage.googleapis.com#1");

        manager.record_pool_outcome(&plan.account, Ok(()));
        let rotated = plan
            .rotate_pool(&pool, Some(manager.as_ref()))
            .await
            .unwrap();
        assert!(rotated);
        assert_eq!(plan.account, "https://generativelanguage.googleapis.com#2");

        manager.record_pool_outcome(&plan.account, Ok(()));
        let rotated = plan
            .rotate_pool(&pool, Some(manager.as_ref()))
            .await
            .unwrap();
        assert!(rotated);
        assert_eq!(plan.account, "https://generativelanguage.googleapis.com");
    }

    #[tokio::test]
    async fn quota_exhaustion_all_keys_in_cooldown_waits_and_recovers() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(1).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        let call_count = Arc::new(AtomicU32::new(0));
        let count_clone = call_count.clone();
        let staging_dir = plan.staging_dir.clone();

        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockSynth {
                key: key.expose().to_string(),
                call_count: count_clone.clone(),
                staging_dir: staging_dir.clone(),
                fail_first_quota: true,
                max_quota_failures: 1, // fails call 0 with 1s cooldown, succeeds call 1
                fail_auth: false,
                fail_retryable_provider: false,
                retry_after: Some(1), // 1s cooldown
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-000".into()),
            &plan.account,
        );

        let run = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-cooldown-recovery",
            Some(manager.as_ref()),
        )
        .await
        .unwrap();

        match run {
            SegmentRun::Done(outcome) => {
                assert!(outcome.duration_ms > 0);
            }
            other => panic!("expected SegmentRun::Done, got {other:?}"),
        }

        // Call 0 failed with 429 quota (1s cooldown), waited for cooldown, call 1 succeeded
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn classification_distinguishes_per_minute_and_daily_limits() {
        let per_minute = ScriptError {
            kind: ScriptErrorKind::Quota,
            retryable: true,
            message: "Quota exceeded: rate limit per minute".to_string(),
            retry_after_secs: Some(30),
            status: Some(429),
        };
        assert_eq!(classify_quota_error(&per_minute), RateLimitClass::PerMinute);

        let default_wait = ScriptError {
            kind: ScriptErrorKind::Quota,
            retryable: true,
            message: "Resource exhausted (HTTP 429)".to_string(),
            retry_after_secs: None,
            status: Some(429),
        };
        assert_eq!(
            classify_quota_error(&default_wait),
            RateLimitClass::PerMinute
        );

        let excessive_wait = ScriptError {
            kind: ScriptErrorKind::Quota,
            retryable: true,
            message: "Resource exhausted (HTTP 429)".to_string(),
            retry_after_secs: Some(301),
            status: Some(429),
        };
        assert_eq!(classify_quota_error(&excessive_wait), RateLimitClass::Daily);

        for daily_msg in [
            "Quota exceeded for metric 'generate_content_requests_per_day'",
            "Free tier limit exceeded",
            "Resource has been exhausted: requests per day limit",
            "RPD limit exceeded",
            "Monthly spend cap reached",
            "Account credit exhausted",
            "Upgrade your billing tier to increase quota",
        ] {
            let daily = ScriptError {
                kind: ScriptErrorKind::Quota,
                retryable: true,
                message: daily_msg.to_string(),
                retry_after_secs: Some(10),
                status: Some(429),
            };
            assert_eq!(
                classify_quota_error(&daily),
                RateLimitClass::Daily,
                "failed to classify daily indicator in: {daily_msg}"
            );
        }
    }

    #[test]
    fn error_sanitizer_redacts_credentials() {
        let msg = "Failed request to https://generativelanguage.googleapis.com/v1beta/models?key=AIzaSyD-Secret12345678901234567890123&test=1 with bearer sk-abcdef12345678901234567890";
        let sanitized = sanitize_error_message(msg);
        assert!(!sanitized.contains("AIzaSyD-Secret12345678901234567890123"));
        assert!(!sanitized.contains("sk-abcdef12345678901234567890"));
        assert!(sanitized.contains("[REDACTED_API_KEY]"));
        assert!(sanitized.contains("key=[REDACTED]"));
    }

    struct MockOverloadSynth {
        key: String,
        call_count: Arc<AtomicU32>,
        staging_dir: std::path::PathBuf,
        fail_overload_count: u32,
        overload_status: u16,
    }

    #[async_trait]
    impl TtsSynth for MockOverloadSynth {
        async fn synthesize(
            &mut self,
            _text: &str,
            _voice: &str,
        ) -> Result<TtsArtifact, ScriptError> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count < self.fail_overload_count {
                return Err(ScriptError {
                    kind: ScriptErrorKind::Provider,
                    retryable: true,
                    message:
                        "This model is currently experiencing high demand. Please try again later."
                            .to_string(),
                    retry_after_secs: None,
                    status: Some(self.overload_status),
                });
            }
            std::fs::create_dir_all(&self.staging_dir).unwrap();
            let audio_path = self
                .staging_dir
                .join(format!("test_overload_{}_{}.wav", self.key, count));
            write_test_wav(&audio_path);
            Ok(TtsArtifact {
                audio_path,
                native_words: None,
            })
        }
    }

    #[tokio::test]
    async fn overload_503_retries_on_same_key_and_succeeds_inside_patience_window() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(2).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        assert_eq!(plan.account, "https://generativelanguage.googleapis.com");
        plan.patience_config = Some(PatienceConfig::for_test());

        let call_count = Arc::new(AtomicU32::new(0));
        let count_clone = call_count.clone();
        let staging_dir = plan.staging_dir.clone();

        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockOverloadSynth {
                key: key.expose().to_string(),
                call_count: count_clone.clone(),
                staging_dir: staging_dir.clone(),
                fail_overload_count: 2,
                overload_status: 503,
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-000".into()),
            &plan.account,
        );

        let run = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-overload-1",
            Some(manager.as_ref()),
        )
        .await
        .unwrap();

        match run {
            SegmentRun::Done(outcome) => {
                assert!(outcome.duration_ms > 0);
            }
            other => panic!("expected SegmentRun::Done, got {other:?}"),
        }

        assert_eq!(plan.account, "https://generativelanguage.googleapis.com");
        assert_eq!(call_count.load(Ordering::SeqCst), 3);

        let keys = manager.list_entries(&pool).await.unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].slot, 0);
        assert_ne!(keys[0].state, KeyState::OutOfQuota);
        assert_eq!(keys[1].slot, 1);
        assert_ne!(keys[1].state, KeyState::OutOfQuota);
    }

    #[tokio::test]
    async fn overload_503_persisting_past_patience_window_pauses_with_service_busy() {
        let (dir, pool, manager, connection, ctx, _control, segment) = setup_test_harness(2).await;
        let mut plan = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap()
        .unwrap_plan();

        assert_eq!(plan.account, "https://generativelanguage.googleapis.com");
        plan.patience_config = Some(PatienceConfig {
            backoff_delays_secs: vec![0],
            max_window_secs: 0,
            pause_wait_secs: 300,
        });

        let call_count = Arc::new(AtomicU32::new(0));
        let count_clone = call_count.clone();
        let staging_dir = plan.staging_dir.clone();

        let factory: TestAdapterFactory = Arc::new(move |key, _account| {
            Box::new(MockOverloadSynth {
                key: key.expose().to_string(),
                call_count: count_clone.clone(),
                staging_dir: staging_dir.clone(),
                fail_overload_count: 5,
                overload_status: 503,
            })
        });

        plan.test_adapter_factory = Some(factory.clone());
        plan.adapter = factory(
            &ApiKey::new("key-material-for-slot-0-000".into()),
            &plan.account,
        );

        let run = synthesize_segment_cancellable(
            &ctx,
            &mut plan,
            &segment,
            "Puck",
            dir.path(),
            "req-key-overload-2",
            Some(manager.as_ref()),
        )
        .await
        .unwrap();

        match run {
            SegmentRun::RateLimited(reason) => {
                assert_eq!(reason.kind, "service_busy");
                assert_eq!(reason.class, RateLimitClass::PerMinute);
                assert!(reason.retry_at_epoch_ms.is_some());
                assert!(reason.last_error_message.contains("busy right now"));
                assert!(!reason.last_error_message.contains('{'));
            }
            other => panic!("expected SegmentRun::RateLimited, got {other:?}"),
        }

        assert_eq!(plan.account, "https://generativelanguage.googleapis.com");
        let keys = manager.list_entries(&pool).await.unwrap();
        assert_eq!(keys[0].state, KeyState::Unchecked);
    }

    #[tokio::test]
    async fn prepare_cloud_tts_pauses_on_entry_when_all_keys_in_quota_cooldown() {
        let (dir, pool, manager, connection, ctx, _control, _segment) = setup_test_harness(2).await;
        let entries = manager.list_entries(&pool).await.unwrap();
        assert_eq!(entries.len(), 2);
        manager.record_dial_quota(&entries[0].account, Some(429), Some(60));
        manager.record_dial_quota(&entries[1].account, Some(429), Some(45));

        let prepared = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap();

        match prepared {
            PreparedCloudTts::Paused(reason) => {
                assert_eq!(reason.kind, "rate_limited");
                assert_eq!(reason.class, RateLimitClass::PerMinute);
                assert!(reason.retry_at_epoch_ms.is_some());
                assert_eq!(reason.attempt, 1);
                assert_eq!(reason.segment_idx, 0);
            }
            PreparedCloudTts::Ready(_) => panic!("expected PreparedCloudTts::Paused, got Ready"),
        }
    }

    #[tokio::test]
    async fn prepare_cloud_tts_re_pause_increments_attempt_on_premature_manual_resume() {
        let (dir, pool, manager, connection, mut ctx, _control, _segment) =
            setup_test_harness(2).await;
        let entries = manager.list_entries(&pool).await.unwrap();
        manager.record_dial_quota(&entries[0].account, Some(429), Some(60));
        manager.record_dial_quota(&entries[1].account, Some(429), Some(45));

        ctx.checkpoint.paused_reason = Some(PausedReason {
            kind: "rate_limited".to_string(),
            class: RateLimitClass::PerMinute,
            retry_at_epoch_ms: Some(now_ms() + 50_000),
            provider: "gemini".to_string(),
            segment_idx: 4,
            last_error_message: "previous quota pause".to_string(),
            attempt: 2,
        });
        ctx.checkpoint.done_segments = 4;

        let prepared = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await
        .unwrap();

        match prepared {
            PreparedCloudTts::Paused(reason) => {
                assert_eq!(reason.kind, "rate_limited");
                assert_eq!(reason.class, RateLimitClass::PerMinute);
                assert_eq!(reason.attempt, 3);
                assert_eq!(reason.segment_idx, 4);
            }
            PreparedCloudTts::Ready(_) => panic!("expected PreparedCloudTts::Paused, got Ready"),
        }
    }

    #[tokio::test]
    async fn prepare_cloud_tts_escalates_to_terminal_quota_after_max_attempts() {
        let (dir, pool, manager, connection, mut ctx, _control, _segment) =
            setup_test_harness(2).await;
        let entries = manager.list_entries(&pool).await.unwrap();
        manager.record_dial_quota(&entries[0].account, Some(429), Some(60));
        manager.record_dial_quota(&entries[1].account, Some(429), Some(45));

        ctx.checkpoint.paused_reason = Some(PausedReason {
            kind: "rate_limited".to_string(),
            class: RateLimitClass::PerMinute,
            retry_at_epoch_ms: Some(now_ms() + 50_000),
            provider: "gemini".to_string(),
            segment_idx: 10,
            last_error_message: "previous quota pause".to_string(),
            attempt: MAX_QUOTA_PAUSE_ATTEMPTS,
        });

        let res = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await;

        match res {
            Err(OrchestratorError::Engine { code, message, .. }) => {
                assert_eq!(code, "ERR_PROVIDER_QUOTA");
                assert!(message.contains("pause attempts exceeded limit"));
            }
            other => panic!("expected Err(ERR_PROVIDER_QUOTA), got {other:?}"),
        }

        let job = job::get_job(&pool, &ctx.job_id).await.unwrap().unwrap();
        let report: serde_json::Value =
            serde_json::from_str(job.report_json.as_deref().unwrap()).unwrap();
        assert_eq!(report["provider_failure"]["class"], "quota");
        assert_eq!(report["provider_failure"]["retryable"], true);
    }

    #[tokio::test]
    async fn prepare_cloud_tts_fails_terminally_with_auth_projection_when_all_keys_invalid() {
        let (dir, pool, manager, connection, ctx, _control, _segment) = setup_test_harness(2).await;
        let entries = manager.list_entries(&pool).await.unwrap();
        manager.record_dial_auth_failure(&entries[0].account, Some(401));
        manager.record_dial_auth_failure(&entries[1].account, Some(401));

        let res = prepare_cloud_tts(
            &pool,
            Some(&manager),
            connection,
            "gemini-2.5-flash",
            dir.path(),
            Some(&ctx),
        )
        .await;

        match res {
            Err(OrchestratorError::Engine { code, message, .. }) => {
                assert_eq!(code, "ERR_PROVIDER_KEY");
                assert!(message.contains("no usable key"));
            }
            other => panic!("expected Err(ERR_PROVIDER_KEY), got {other:?}"),
        }

        let job = job::get_job(&pool, &ctx.job_id).await.unwrap().unwrap();
        let report: serde_json::Value =
            serde_json::from_str(job.report_json.as_deref().unwrap()).unwrap();
        assert_eq!(report["provider_failure"]["class"], "auth");
        assert_eq!(report["provider_failure"]["retryable"], false);
    }
}
