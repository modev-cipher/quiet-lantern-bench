//! The cloud Script wired path (provider-flexibility charter S-1d; the S-5
//! dev flag retirement made per-connection enablement the release gate).
//!
//! A job whose `model_overrides_json` carries a structural
//! `"script_binding"` `PhaseBinding` (charter §4.4) resolves here: units are
//! assembled by the engine's local `/script/units` seam, the ported teacher
//! contract (`providers::script`) runs the provider ladder in Rust around
//! the recorded connection's adapter, and the outcomes go back through the
//! engine's `/script/segments` seam for pysbd segmentation. Keys resolve
//! only inside this module, from VA3 custody, straight into the adapter
//! (charter §3.3 law 3).
//!
//! Selection is gated by per-connection enablement (S-5): a disabled
//! connection refuses BEFORE custody or network. Refusal is a stage error,
//! never a silent fall-back to Local — a recorded cloud binding must not be
//! re-resolved to something else by configuration state (charter T18,
//! checklist 34).

use std::sync::Arc;

use super::overload::{
    friendly_provider_error, PatienceConfig, PAUSED_KIND_RATE_LIMITED, PAUSED_KIND_SERVICE_BUSY,
};
use super::provider_error_policy::{FaultClass, PatienceBudget, ProviderErrorPolicy};
use super::scripting::SidecarProvider;
use super::stage::StageContext;
use super::stage::StageOutcome;
use super::state::{Checkpoint, OrchestratorError, PausedReason, RateLimitClass};
use super::synthesis_cloud::MAX_AUTO_RESUME_WAIT_SECS;
use crate::providers::registry::{PhaseCapability, ScriptError, ScriptErrorKind, ScriptLlm};
use crate::providers::script::chunker::SourceUnit;
use crate::providers::script::teacher::{teach_chunks, ChunkOutcome};
use crate::providers::{ConnectionRecord, ConnectionStore, PhaseBinding, ProviderKind};
use crate::sidecar::client::post_json;
use crate::storage::{job, now_ms};
use crate::voice::keys::KeyManager;
use sha2::Digest;

/// Chunk-budget context figure for cloud Script runs.
///
/// The local path budgets chunks from the loaded GGUF's real window; a cloud
/// model's window is provider-side and the charter (§6.1) bans a user ctx
/// knob outright. This is an internal safety constant, not configuration.
/// 32k is far under the fixed Gemini adapter's window (gemini-3.7-flash,
/// the current flagship Flash: 1,048,576 input / 65,536 output tokens —
/// https://ai.google.dev/gemini-api/docs/models/gemini-3.7-flash, checked
/// 2026-08-28; the older 2.5-flash window is identical). An OpenAI-compatible endpoint's window is whatever the user
/// pointed the connection at and cannot be known here; a model with a
/// smaller window degrades through the recorded script contract itself —
/// over-budget requests fail or come back malformed, walk the repair
/// ladder, and land in the recorded per-chunk narrator fallback (A19) —
/// never through a silent binding change.
pub const CLOUD_SCRIPT_CTX: usize = 32_768;
pub const MAX_OVERLOAD_PAUSE_ATTEMPTS: u32 = 5;
pub const MAX_RATE_LIMIT_PAUSE_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottlePause {
    Pause(u32),
    Exhausted,
}

pub fn throttle_pause(checkpoint: &mut Checkpoint, done_now: i64, cap: u32) -> ThrottlePause {
    if done_now > checkpoint.last_pause_done {
        checkpoint.rate_limit_pauses = 0;
    }
    checkpoint.last_pause_done = done_now;
    checkpoint.rate_limit_pauses += 1;
    let attempt = checkpoint.rate_limit_pauses;
    if attempt > cap {
        ThrottlePause::Exhausted
    } else {
        ThrottlePause::Pause(attempt)
    }
}

pub fn auto_resume_class(wait_secs: i64) -> RateLimitClass {
    if wait_secs > MAX_AUTO_RESUME_WAIT_SECS as i64 {
        RateLimitClass::Daily
    } else {
        RateLimitClass::PerMinute
    }
}

/// The structural per-job Script binding, read from the same
/// `model_overrides_json` document the legacy `"llm"` local override lives
/// in, under `"script_binding"` (charter §4.4: local vs connection is a
/// tagged shape, never an ambiguous bare string).
///
/// A document that does not parse as JSON is treated as carrying no binding
/// — that matches `override_llm_id`'s long-standing leniency for the other
/// keys in the same document. A document that parses and HAS the key but
/// whose value is not a valid `PhaseBinding` refuses loudly instead: a
/// recorded cloud selection must never silently degrade to Local because of
/// a shape drift (charter T18).
pub fn override_script_binding(
    raw: Option<&str>,
) -> Result<Option<PhaseBinding>, OrchestratorError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(None);
    };
    let Some(binding) = value.get("script_binding") else {
        return Ok(None);
    };
    serde_json::from_value::<PhaseBinding>(binding.clone())
        .map(Some)
        .map_err(|err| {
            OrchestratorError::InvalidRequest(format!(
                "this job's recorded script_binding does not parse ({err}); refusing to \
                 re-resolve a recorded binding — fix or clear the binding explicitly"
            ))
        })
}

pub(super) fn provider_refusal(message: String) -> OrchestratorError {
    OrchestratorError::Engine {
        status: None,
        code: "ERR_PROVIDER".to_string(),
        message,
    }
}

pub(super) fn script_error_code(kind: ScriptErrorKind) -> &'static str {
    match kind {
        ScriptErrorKind::Auth => "ERR_PROVIDER_AUTH",
        ScriptErrorKind::Quota => "ERR_PROVIDER_QUOTA",
        ScriptErrorKind::Transport => "ERR_PROVIDER_TRANSPORT",
        ScriptErrorKind::Timeout => "ERR_PROVIDER_TIMEOUT",
        ScriptErrorKind::Provider => "ERR_PROVIDER",
        ScriptErrorKind::ModelNotFound => "ERR_PROVIDER_MODEL",
        ScriptErrorKind::Malformed => "ERR_PROVIDER_MALFORMED",
        ScriptErrorKind::Cancelled => "ERR_CANCELLED",
    }
}

/// Build the recorded connection's adapter with its key resolved from VA3
/// custody. The recorded binding's model is used verbatim — never the
/// connection's (possibly since-edited) `default_model` (charter §4.4:
/// resume/retry never re-resolves against changed settings).
/// Returns the adapter plus the custody account the key came from (None for
/// lawful keyless): the caller feeds settled provider outcomes back into
/// slot state through it (S-5 blocker 5 — rotation learns outcomes).
async fn build_adapter(
    pool: &sqlx::SqlitePool,
    keys: Option<&Arc<KeyManager>>,
    connection: &ConnectionRecord,
    model: &str,
) -> Result<(Box<dyn ScriptLlm>, Option<String>), OrchestratorError> {
    let mut account: Option<String> = None;
    let key = if connection.credential_not_required {
        // Re-check the A1 keyless law at the request seam, not just at store
        // write time: `ConnectionStore::list` deserializes without
        // revalidation, so a hand-edited settings row could otherwise reach
        // here as keyless-with-a-remote-host and send an unauthenticated
        // request off the machine.
        let keyless_lawful =
            matches!(
                connection.provider.credential_policy(),
                crate::providers::registry::CredentialPolicy::KeyOrLoopbackKeyless
            ) && crate::voice::keys::policy::is_loopback_base_url(&connection.key_base_url());
        if !keyless_lawful {
            return Err(provider_refusal(format!(
                "connection '{}' is stored as credential-not-required but is not a loopback \
                 OpenAI-compatible endpoint; keyless is lawful only there (A1) — add a key or \
                 re-create the connection",
                connection.name
            )));
        }
        None
    } else {
        let manager = keys.ok_or_else(|| {
            provider_refusal(
                "key custody is unavailable in this build; a keyed cloud binding cannot run"
                    .to_string(),
            )
        })?;
        Some(
            manager
                // Provider-plus-canonical-URL pool custody (PF-PH1): the
                // key comes from THIS provider's pool at THIS endpoint via
                // the shared VA3 rotation — a slot stored under another
                // provider never matches, however URLs coincide.
                .resolve_pool_key(
                    pool,
                    connection.provider.vault_provider().id(),
                    &connection.key_base_url(),
                )
                .await
                .map(|candidate| {
                    account = Some(candidate.account.clone());
                    candidate.key
                })
                .map_err(|err| OrchestratorError::Engine {
                    status: None,
                    code: "ERR_PROVIDER_KEY".to_string(),
                    message: format!(
                        "no usable key for connection '{}': {err}; add a key or choose Local",
                        connection.name
                    ),
                })?,
        )
    };
    let adapter: Box<dyn ScriptLlm> = match connection.provider {
        ProviderKind::OpenaiCompatible => Box::new(
            crate::providers::adapters::openai_compat::OpenAiCompatScriptLlm::new(
                connection.key_base_url(),
                key,
                model.to_string(),
            )
            .map_err(|err| provider_refusal(err.message))?,
        ),
        ProviderKind::Gemini => Box::new(
            crate::providers::adapters::gemini::GeminiScriptLlm::new(
                key.ok_or_else(|| {
                    provider_refusal("a Gemini connection requires a key".to_string())
                })?,
                model.to_string(),
            )
            .map_err(|err| provider_refusal(err.message))?,
        ),
        ProviderKind::Anthropic => Box::new(
            crate::providers::adapters::anthropic::AnthropicScriptLlm::new(
                key.ok_or_else(|| {
                    provider_refusal("an Anthropic connection requires a key".to_string())
                })?,
                model.to_string(),
            )
            .map_err(|err| provider_refusal(err.message))?,
        ),
        // TTS-only kinds never reach here — resolve_connection's
        // servable-phase check refuses them first; this arm exists so the
        // match stays exhaustive and a future kind is a compile error.
        ProviderKind::CartesiaTts | ProviderKind::OpenaiTts => {
            return Err(provider_refusal(format!(
                "provider '{}' cannot serve the Script phase",
                connection.provider.id()
            )))
        }
    };
    Ok((adapter, account))
}

/// Load and vet the recorded connection. Disabled connections refuse with
/// the remedy spelled out (charter A29): disable never mutates a recorded
/// binding, and a refusal here is the "resuming asks" half of that law.
pub async fn resolve_connection(
    pool: &sqlx::SqlitePool,
    connection_id: &str,
    phase: PhaseCapability,
) -> Result<ConnectionRecord, OrchestratorError> {
    // S-5: the dev flag is retired — release readiness IS per-connection
    // enablement plus PF-PH1 provider-and-endpoint pool custody.
    let connection = ConnectionStore::get(pool, connection_id)
        .await
        .map_err(|err| {
            provider_refusal(format!(
                "the recorded connection cannot be loaded: {err}; re-create it or choose Local \
                 explicitly — a recorded binding is never silently replaced"
            ))
        })?;
    if !connection.enabled {
        return Err(provider_refusal(format!(
            "connection '{}' is disabled; re-enable it or choose Local explicitly",
            connection.name
        )));
    }
    if !connection.provider.servable_phases().contains(&phase) {
        return Err(provider_refusal(format!(
            "provider '{}' cannot serve this phase",
            connection.provider.id()
        )));
    }
    Ok(connection)
}

/// T18 snapshot enforcement (pre-brief item 1): the recorded binding's
/// provider kind, canonical endpoint, and adapter revision must still hold
/// on the LIVE connection — an edited base URL or changed adapter refuses
/// with the remedy rather than silently redirecting unfinished work.
pub(super) fn enforce_snapshot(
    connection: &ConnectionRecord,
    provider: &str,
    endpoint: &str,
    adapter_rev: &str,
) -> Result<(), OrchestratorError> {
    if connection.provider.id() != provider {
        return Err(provider_refusal(format!(
            "this job was planned against provider '{provider}' but connection '{}' is now \
             '{}'; re-select explicitly",
            connection.name,
            connection.provider.id()
        )));
    }
    if connection.key_base_url().trim_end_matches('/') != endpoint.trim_end_matches('/') {
        return Err(provider_refusal(format!(
            "this job was planned against {endpoint} but connection '{}' now points at {}; \
             a rebound endpoint never silently receives unfinished work — re-select explicitly",
            connection.name,
            connection.key_base_url()
        )));
    }
    if connection.provider.adapter_revision() != adapter_rev {
        return Err(provider_refusal(format!(
            "this job was planned under adapter revision {adapter_rev} but this build runs {}; \
             re-plan the job explicitly",
            connection.provider.adapter_revision()
        )));
    }
    Ok(())
}

pub(super) fn enforce_narration_snapshot(
    connection: &ConnectionRecord,
    provider: &str,
    endpoint: &str,
    adapter_rev: &str,
    model: &str,
) -> Result<(), OrchestratorError> {
    enforce_snapshot(
        connection,
        provider,
        endpoint,
        connection.provider.adapter_revision(),
    )?;
    if connection.provider.narration_revision(model) != adapter_rev {
        return Err(provider_refusal(
            "the recorded narration adapter revision is unavailable; re-select explicitly"
                .to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct UnitsResponse {
    units: Vec<SourceUnit>,
}

#[derive(Debug, serde::Deserialize)]
struct SegmentsResponse {
    segments: Vec<serde_json::Value>,
}

pub struct CloudScriptOutput {
    pub segments: Vec<serde_json::Value>,
    pub markers_dropped: u64,
    pub markers_omitted: u64,
    pub done_chunks: Option<i64>,
}

pub enum CloudScriptRun {
    Completed(CloudScriptOutput),
    Interrupted(StageOutcome),
    RateLimited(PausedReason),
}

/// Applies the job's recorded per-phase effort (F6 override) to every
/// request the teacher contract issues; adapters keep mapping it to their
/// provider's documented parameter and omitting it where undocumented.
#[async_trait::async_trait]
pub(super) trait AdapterFactory: Send + Sync {
    async fn rebuild(&self) -> Result<(Box<dyn ScriptLlm>, Option<String>), OrchestratorError>;
}

pub(super) struct PoolAdapterFactory {
    pool: crate::storage::SqlitePool,
    keys: Option<Arc<KeyManager>>,
    connection: ConnectionRecord,
    model: String,
    effort: crate::providers::registry::ReasoningEffort,
}

#[async_trait::async_trait]
impl AdapterFactory for PoolAdapterFactory {
    async fn rebuild(&self) -> Result<(Box<dyn ScriptLlm>, Option<String>), OrchestratorError> {
        let (inner, account) = build_adapter(
            &self.pool,
            self.keys.as_ref(),
            &self.connection,
            &self.model,
        )
        .await?;
        Ok((
            Box::new(RecordedEffort {
                inner,
                effort: self.effort,
            }),
            account,
        ))
    }
}

struct RecordedEffort {
    inner: Box<dyn ScriptLlm>,
    effort: crate::providers::registry::ReasoningEffort,
}

#[async_trait::async_trait]
impl ScriptLlm for RecordedEffort {
    async fn generate(
        &mut self,
        request: crate::providers::registry::ScriptRequest,
    ) -> Result<crate::providers::registry::ScriptResponse, ScriptError> {
        self.inner
            .generate(crate::providers::registry::ScriptRequest {
                effort: self.effort,
                ..request
            })
            .await
    }
}

/// The wired path: engine unit assembly → Rust teacher contract over the
/// bound adapter → engine segmentation. `body` is the exact scope/pages
/// payload the local `/script` path would have sent.
#[allow(clippy::too_many_arguments)]
pub async fn run_cloud_script(
    ctx: &mut StageContext,
    client: &reqwest::Client,
    sidecar: &dyn SidecarProvider,
    keys: Option<&Arc<KeyManager>>,
    binding: &PhaseBinding,
    mode: &str,
    body: serde_json::Value,
    patience_config: Option<PatienceConfig>,
) -> Result<CloudScriptRun, OrchestratorError> {
    let PhaseBinding::Connection {
        connection_id,
        model,
        provider,
        endpoint,
        adapter_rev,
        effort,
    } = binding
    else {
        return Err(OrchestratorError::InvalidRequest(
            "run_cloud_script requires a connection binding".to_string(),
        ));
    };
    if mode != "teacher" {
        return Err(OrchestratorError::InvalidRequest(
            "cloud script bindings currently serve teacher mode only; narrator normalization \
             stays local"
                .to_string(),
        ));
    }
    let connection =
        resolve_connection(&ctx.pool, connection_id, PhaseCapability::ScriptLlm).await?;
    enforce_snapshot(&connection, provider, endpoint, adapter_rev)?;
    ctx.emit_progress(
        0.0,
        None,
        &format!(
            "writing the script with {}",
            super::overload::connection_label(&connection)
        ),
    );
    let (adapter, dial_account) = build_adapter(&ctx.pool, keys, &connection, model).await?;
    let mut adapter: Box<dyn ScriptLlm> = Box::new(RecordedEffort {
        inner: adapter,
        effort: *effort,
    });

    let (port, token) = match super::scripting::await_connection(sidecar, &ctx.control).await? {
        super::scripting::ConnectionWait::Ready(port, token) => (port, token),
        super::scripting::ConnectionWait::Interrupted(outcome) => {
            return Ok(CloudScriptRun::Interrupted(outcome))
        }
    };
    tracing::info!(
        job = %ctx.job_id,
        connection = %connection.id,
        provider = connection.provider.id(),
        model,
        "stage SCRIPTING running the cloud teacher contract over the recorded binding"
    );

    let units: UnitsResponse = post_json(client, port, &token, "/script/units", &body)
        .await
        .map_err(OrchestratorError::from)?;

    // MF11 never-re-pay: chunking is deterministic over (units, ctx), so a
    // fingerprint of the units plus the chunk count ties persisted outcomes
    // to exactly this source; a mismatch (changed book/scope) discards them
    // — correctness over economy, loudly logged — rather than resuming
    // against different text.
    let units_fingerprint = format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_string(&units.units)?.as_bytes())
    );
    let total_chunks = crate::providers::script::chunker::chunk_units(
        &units.units,
        crate::providers::script::teacher::teacher_chunk_budget_chars(CLOUD_SCRIPT_CTX),
    )
    .len() as i64;
    let mut resume: Vec<crate::providers::script::teacher::CompletedChunk> = Vec::new();
    let rows = job::list_script_chunk_checkpoints(&ctx.pool, &ctx.job_id).await?;
    let rows_valid = !rows.is_empty()
        && rows.iter().all(|row| {
            row.units_fingerprint == units_fingerprint && row.total_chunks == total_chunks
        })
        && rows
            .iter()
            .enumerate()
            .all(|(position, row)| row.chunk_idx == position as i64);
    if rows_valid {
        for row in &rows {
            let outcomes: Vec<ChunkOutcome> = serde_json::from_str(&row.outcome_json)?;
            if outcomes.is_empty() {
                // Belt to the writer-side guard: an empty "completed" chunk
                // is content loss waiting to happen — discard the whole set.
                resume.clear();
                break;
            }
            resume.push(crate::providers::script::teacher::CompletedChunk {
                chunk_idx: row.chunk_idx as usize,
                outcomes,
                summary: row.summary.clone(),
            });
        }
        tracing::info!(
            job = %ctx.job_id,
            resumed_chunks = resume.len(),
            "resuming the cloud script from durable chunk checkpoints; completed chunks              are never re-requested"
        );
    } else if !rows.is_empty() {
        tracing::warn!(
            job = %ctx.job_id,
            "discarding {} stale script chunk checkpoint(s) (source/config changed)",
            rows.len()
        );
        job::delete_script_chunk_checkpoints(&ctx.pool, &ctx.job_id).await?;
    }

    let factory = PoolAdapterFactory {
        pool: ctx.pool.clone(),
        keys: keys.map(Arc::clone),
        connection: connection.clone(),
        model: model.to_string(),
        effort: *effort,
    };

    let loop_outcome = run_cloud_script_teacher_loop(
        ctx,
        &units.units,
        &units_fingerprint,
        total_chunks,
        resume,
        &mut adapter,
        &connection,
        dial_account,
        keys,
        patience_config.unwrap_or_default(),
        &factory,
    )
    .await?;

    let (outcomes, done_chunks) = match loop_outcome {
        TeacherLoopOutcome::Interrupted(outcome) => {
            return Ok(CloudScriptRun::Interrupted(outcome))
        }
        TeacherLoopOutcome::RateLimited(reason) => return Ok(CloudScriptRun::RateLimited(reason)),
        TeacherLoopOutcome::Completed {
            outcomes,
            done_chunks,
        } => (outcomes, done_chunks),
    };

    let mut markers_dropped = 0u64;
    let mut markers_omitted = 0u64;
    let chunks: Vec<serde_json::Value> = outcomes
        .iter()
        .map(|outcome| match outcome {
            ChunkOutcome::Lecture {
                units,
                texts,
                markers_dropped: dropped,
                markers_omitted: omitted,
            } => {
                markers_dropped += dropped;
                markers_omitted += omitted;
                serde_json::json!({ "units": units, "texts": texts })
            }
            ChunkOutcome::NarratorFallback { units } => {
                serde_json::json!({ "units": units, "texts": serde_json::Value::Null })
            }
        })
        .collect();
    let segments: SegmentsResponse = post_json(
        client,
        port,
        &token,
        "/script/segments",
        &serde_json::json!({ "chunks": chunks }),
    )
    .await
    .map_err(OrchestratorError::from)?;

    // The chunks are settled into segments; the per-chunk ledger has served
    // its purpose and must not leak into a later job state.
    job::delete_script_chunk_checkpoints(&ctx.pool, &ctx.job_id).await?;
    Ok(CloudScriptRun::Completed(CloudScriptOutput {
        segments: segments.segments,
        markers_dropped,
        markers_omitted,
        done_chunks: if done_chunks >= 0 {
            Some(done_chunks)
        } else {
            None
        },
    }))
}

#[derive(Debug)]
pub(super) enum TeacherLoopOutcome {
    Completed {
        outcomes: Vec<ChunkOutcome>,
        done_chunks: i64,
    },
    Interrupted(StageOutcome),
    RateLimited(PausedReason),
}

async fn reload_script_chunk_checkpoints(
    pool: &crate::storage::SqlitePool,
    job_id: &str,
    units_fingerprint: &str,
    total_chunks: i64,
    resume: &mut Vec<crate::providers::script::teacher::CompletedChunk>,
) {
    if let Ok(fresh_rows) = job::list_script_chunk_checkpoints(pool, job_id).await {
        resume.clear();
        for row in fresh_rows {
            if row.units_fingerprint == units_fingerprint && row.total_chunks == total_chunks {
                if let Ok(outcomes) = serde_json::from_str::<Vec<ChunkOutcome>>(&row.outcome_json) {
                    if !outcomes.is_empty() {
                        resume.push(crate::providers::script::teacher::CompletedChunk {
                            chunk_idx: row.chunk_idx as usize,
                            outcomes,
                            summary: row.summary.clone(),
                        });
                    }
                }
            }
        }
        resume.sort_by_key(|c| c.chunk_idx);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_cloud_script_teacher_loop(
    ctx: &mut StageContext,
    units: &[SourceUnit],
    units_fingerprint: &str,
    total_chunks: i64,
    mut resume: Vec<crate::providers::script::teacher::CompletedChunk>,
    adapter: &mut Box<dyn ScriptLlm>,
    connection: &ConnectionRecord,
    mut dial_account: Option<String>,
    keys: Option<&Arc<KeyManager>>,
    patience: PatienceConfig,
    factory: &dyn AdapterFactory,
) -> Result<TeacherLoopOutcome, OrchestratorError> {
    let done_chunks = std::sync::atomic::AtomicI64::new(-1);
    let mut overload_attempts: usize = 0;
    let mut overload_start: Option<tokio::time::Instant> = None;

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
        if let Some(outcome) = ctx.control.interruption() {
            return Ok(TeacherLoopOutcome::Interrupted(outcome));
        }

        // Durable persistence at each safe boundary, off the generation path.
        let (checkpoint_tx, mut checkpoint_rx) = tokio::sync::mpsc::unbounded_channel::<
            crate::providers::script::teacher::CompletedChunk,
        >();
        let persister = tokio::spawn({
            let pool = ctx.pool.clone();
            let job_id = ctx.job_id.clone();
            let units_fingerprint = units_fingerprint.to_string();
            async move {
                while let Some(completed) = checkpoint_rx.recv().await {
                    let outcome_json = match serde_json::to_string(&completed.outcomes) {
                        Ok(json) if !completed.outcomes.is_empty() => json,
                        Ok(_) | Err(_) => {
                            tracing::warn!(
                                chunk = completed.chunk_idx,
                                "chunk checkpoint not persisted (unserializable or empty \
                                 outcomes); resume will re-generate this chunk"
                            );
                            continue;
                        }
                    };
                    let record = job::ScriptChunkCheckpoint {
                        chunk_idx: completed.chunk_idx as i64,
                        units_fingerprint: units_fingerprint.clone(),
                        total_chunks,
                        outcome_json,
                        summary: completed.summary,
                    };
                    if let Err(error) =
                        job::upsert_script_chunk_checkpoint(&pool, &job_id, &record).await
                    {
                        tracing::warn!("script chunk checkpoint write failed: {error}");
                    }
                }
            }
        });

        // Bridge the job control into the provider-neutral token so an
        // IN-FLIGHT generation aborts on pause/cancel (dropped future = aborted
        // request), not just the next chunk boundary.
        let (cancel_handle, mut cancel_token) = crate::providers::registry::cancel_channel();
        let bridge = tokio::spawn({
            let mut watcher = ctx.control.clone();
            async move {
                watcher.interrupted().await;
                cancel_handle.cancel();
            }
        });

        let res = teach_chunks(
            units,
            CLOUD_SCRIPT_CTX,
            adapter.as_mut(),
            resume.clone(),
            |done, total| {
                done_chunks.store(done as i64, std::sync::atomic::Ordering::SeqCst);
                if total > 0 {
                    ctx.emit_progress(
                        done as f64 / total as f64,
                        None,
                        &format!("chunk {done}/{total}"),
                    );
                }
            },
            &mut cancel_token,
            |completed| {
                let _ = checkpoint_tx.send(completed.clone());
            },
        )
        .await;

        bridge.abort();
        drop(checkpoint_tx);
        let _ = persister.await;

        match res {
            Ok(outcomes) => {
                if let (Some(account), Some(manager)) = (&dial_account, keys) {
                    manager.record_pool_outcome(account, Ok(()));
                }
                return Ok(TeacherLoopOutcome::Completed {
                    outcomes,
                    done_chunks: done_chunks.load(std::sync::atomic::Ordering::SeqCst),
                });
            }
            Err(ScriptError {
                kind: ScriptErrorKind::Cancelled,
                ..
            }) => {
                let outcome = ctx
                    .control
                    .interruption()
                    .unwrap_or(StageOutcome::Cancelled);
                return Ok(TeacherLoopOutcome::Interrupted(outcome));
            }
            Err(error) => {
                let fault = ProviderErrorPolicy::classify_script_error(&error);
                match fault {
                    FaultClass::ClusterOverload { .. } | FaultClass::TransientTransport => {
                        let retry_after_secs = match fault {
                            FaultClass::ClusterOverload { retry_after_secs } => retry_after_secs,
                            _ => None,
                        };
                        // Key remains Active: do NOT demote, do NOT rotate cursor.
                        // Exponential backoff with full jitter on the SAME key.
                        if overload_start.is_none() {
                            overload_start = Some(tokio::time::Instant::now());
                        }
                        let elapsed = overload_start.unwrap().elapsed();
                        if overload_attempts < budget.max_cluster_attempts
                            && (patience.max_window_secs == 0
                                || elapsed.as_secs() < patience.max_window_secs)
                            && !(patience.max_window_secs == 0 && patience.pause_wait_secs > 1)
                        {
                            overload_attempts += 1;
                            let delay = budget.calculate_delay(overload_attempts, retry_after_secs);
                            let delay_secs = delay.as_secs_f64();
                            tracing::warn!(
                                job = %ctx.job_id,
                                provider = %connection.name,
                                attempt = overload_attempts,
                                max_attempts = budget.max_cluster_attempts,
                                delay_secs,
                                is_overnight = budget.is_overnight,
                                "cloud script provider overloaded / transient transport; retrying on same key with jittered backoff"
                            );

                            ctx.emit_progress(
                                done_chunks.load(std::sync::atomic::Ordering::SeqCst).max(0) as f64
                                    / total_chunks.max(1) as f64,
                                None,
                                &format!(
                                    "Provider busy, retrying in {:.0}s... (attempt {}/{})",
                                    delay_secs, overload_attempts, budget.max_cluster_attempts
                                ),
                            );

                            let mut sleep_watcher = ctx.control.clone();
                            tokio::select! {
                                outcome = sleep_watcher.interrupted() => {
                                    return Ok(TeacherLoopOutcome::Interrupted(outcome));
                                }
                                _ = tokio::time::sleep(delay) => {}
                            }

                            reload_script_chunk_checkpoints(
                                &ctx.pool,
                                &ctx.job_id,
                                units_fingerprint,
                                total_chunks,
                                &mut resume,
                            )
                            .await;
                            continue;
                        }

                        // Window or attempts elapsed!
                        match throttle_pause(
                            &mut ctx.checkpoint,
                            done_chunks.load(std::sync::atomic::Ordering::SeqCst).max(0),
                            MAX_OVERLOAD_PAUSE_ATTEMPTS,
                        ) {
                            ThrottlePause::Exhausted => {
                                return Err(provider_refusal(format!(
                                    "provider '{}' remained overloaded across {MAX_OVERLOAD_PAUSE_ATTEMPTS} pause attempts; pausing exhausted",
                                    connection.name
                                )));
                            }
                            ThrottlePause::Pause(attempt) => {
                                let wait_secs =
                                    retry_after_secs.unwrap_or(patience.pause_wait_secs);
                                let class = auto_resume_class(wait_secs as i64);
                                let retry_at_epoch_ms = match class {
                                    RateLimitClass::PerMinute => {
                                        Some(now_ms() + (wait_secs as i64 * 1000))
                                    }
                                    RateLimitClass::Daily => None,
                                };
                                let friendly_msg = friendly_provider_error(
                                    &connection.name,
                                    error.status,
                                    &error.message,
                                );
                                let reason = PausedReason {
                                    kind: PAUSED_KIND_SERVICE_BUSY.to_string(),
                                    class,
                                    retry_at_epoch_ms,
                                    provider: connection.provider.vault_provider().id().to_string(),
                                    segment_idx: done_chunks
                                        .load(std::sync::atomic::Ordering::SeqCst)
                                        .max(0),
                                    last_error_message: friendly_msg,
                                    attempt,
                                };
                                tracing::warn!(
                                    job = %ctx.job_id,
                                    provider = %connection.name,
                                    retry_at_epoch_ms = ?retry_at_epoch_ms,
                                    attempt,
                                    "cloud script provider overloaded past patience window; pausing job honestly"
                                );
                                return Ok(TeacherLoopOutcome::RateLimited(reason));
                            }
                        }
                    }
                    FaultClass::KeyAuthInvalid => {
                        if let (Some(account), Some(manager)) = (&dial_account, keys) {
                            manager.record_dial_auth_failure(account, None);
                            manager.advance_pool_cursor(account);
                            if let Ok((new_adapter, new_account)) = factory.rebuild().await {
                                tracing::info!(
                                    job = %ctx.job_id,
                                    old_key = %account,
                                    new_key = new_account.as_deref().unwrap_or("?"),
                                    "rotated to next pool key after auth invalid; continuing without sleep"
                                );
                                *adapter = new_adapter;
                                dial_account = new_account;
                                overload_attempts = 0;
                                reload_script_chunk_checkpoints(
                                    &ctx.pool,
                                    &ctx.job_id,
                                    units_fingerprint,
                                    total_chunks,
                                    &mut resume,
                                )
                                .await;
                                continue;
                            }
                        }
                    }
                    FaultClass::KeyRateLimited { retry_after_secs } => {
                        if let (Some(account), Some(manager)) = (&dial_account, keys) {
                            manager.record_dial_quota(account, None, retry_after_secs);
                            manager.advance_pool_cursor(account);

                            if let Ok((new_adapter, new_account)) = factory.rebuild().await {
                                tracing::info!(
                                    job = %ctx.job_id,
                                    old_key = %account,
                                    new_key = new_account.as_deref().unwrap_or("?"),
                                    "rotated to next pool key after 429 rate limit; continuing without sleep"
                                );
                                *adapter = new_adapter;
                                dial_account = new_account;
                                overload_attempts = 0;
                                reload_script_chunk_checkpoints(
                                    &ctx.pool,
                                    &ctx.job_id,
                                    units_fingerprint,
                                    total_chunks,
                                    &mut resume,
                                )
                                .await;
                                continue;
                            }
                        }

                        if overload_attempts < budget.max_cluster_attempts {
                            let wait_secs = if budget.max_backoff_cap_secs <= 0.0 {
                                0
                            } else {
                                retry_after_secs.unwrap_or(60).min(if budget.is_overnight {
                                    300
                                } else {
                                    60
                                })
                            };
                            overload_attempts += 1;
                            tracing::warn!(
                                job = %ctx.job_id,
                                provider = %connection.name,
                                attempt = overload_attempts,
                                wait_secs,
                                is_overnight = budget.is_overnight,
                                "cloud script rate-limited; cooling down before retry"
                            );
                            let mut sleep_watcher = ctx.control.clone();
                            tokio::select! {
                                outcome = sleep_watcher.interrupted() => {
                                    return Ok(TeacherLoopOutcome::Interrupted(outcome));
                                }
                                _ = tokio::time::sleep(std::time::Duration::from_secs(wait_secs)) => {}
                            }

                            reload_script_chunk_checkpoints(
                                &ctx.pool,
                                &ctx.job_id,
                                units_fingerprint,
                                total_chunks,
                                &mut resume,
                            )
                            .await;
                            continue;
                        }

                        match throttle_pause(
                            &mut ctx.checkpoint,
                            done_chunks.load(std::sync::atomic::Ordering::SeqCst).max(0),
                            MAX_RATE_LIMIT_PAUSE_ATTEMPTS,
                        ) {
                            ThrottlePause::Exhausted => {
                                return Err(provider_refusal(format!(
                                    "provider '{}' stayed rate-limited across {MAX_RATE_LIMIT_PAUSE_ATTEMPTS} pause attempts; pausing exhausted",
                                    connection.name
                                )));
                            }
                            ThrottlePause::Pause(attempt) => {
                                let wait_secs =
                                    retry_after_secs.unwrap_or(patience.pause_wait_secs);
                                let class = auto_resume_class(wait_secs as i64);
                                let retry_at_epoch_ms = match class {
                                    RateLimitClass::PerMinute => {
                                        Some(now_ms() + (wait_secs as i64 * 1000))
                                    }
                                    RateLimitClass::Daily => None,
                                };
                                let friendly_msg = friendly_provider_error(
                                    &connection.name,
                                    error.status,
                                    &error.message,
                                );
                                let reason = PausedReason {
                                    kind: PAUSED_KIND_RATE_LIMITED.to_string(),
                                    class,
                                    retry_at_epoch_ms,
                                    provider: connection.provider.vault_provider().id().to_string(),
                                    segment_idx: done_chunks
                                        .load(std::sync::atomic::Ordering::SeqCst)
                                        .max(0),
                                    last_error_message: friendly_msg,
                                    attempt,
                                };
                                return Ok(TeacherLoopOutcome::RateLimited(reason));
                            }
                        }
                    }
                    FaultClass::Terminal => {}
                }

                // Terminal failure:
                if let (Some(account), Some(manager)) = (&dial_account, keys) {
                    manager.record_pool_outcome(account, Err(&error));
                }

                let projection =
                    crate::providers::registry::ProviderFailure::from_script_error(&error);
                if let Ok(json) = serde_json::to_value(&projection) {
                    if let Err(report_error) =
                        ctx.merge_report_value("provider_failure", json).await
                    {
                        tracing::warn!("provider failure projection not recorded: {report_error}");
                    }
                }
                let friendly_msg =
                    friendly_provider_error(&connection.name, error.status, &error.message);
                return Err(OrchestratorError::Engine {
                    status: None,
                    code: script_error_code(error.kind).to_string(),
                    message: format!(
                        "cloud script over connection '{}' failed: {}; the binding is preserved — \
                         retry, fix the connection, or choose Local explicitly",
                        connection.name, friendly_msg
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_unrelated_overrides_carry_no_binding() {
        assert_eq!(override_script_binding(None).unwrap(), None);
        assert_eq!(
            override_script_binding(Some(r#"{"llm":"phi-4-mini"}"#)).unwrap(),
            None
        );
        // Unparseable documents keep the legacy leniency of the other keys.
        assert_eq!(override_script_binding(Some("not json")).unwrap(), None);
    }

    #[test]
    fn a_structural_binding_parses_both_kinds() {
        let local = override_script_binding(Some(
            r#"{"script_binding":{"kind":"local","model_id":"phi-4-mini"}}"#,
        ))
        .unwrap();
        assert_eq!(
            local,
            Some(PhaseBinding::Local {
                model_id: "phi-4-mini".to_string()
            })
        );
        let cloud = override_script_binding(Some(
            r#"{"script_binding":{"kind":"connection","connection_id":"c-1","model":"m-9","provider":"gemini","endpoint":"https://generativelanguage.googleapis.com","adapter_rev":"gemini/1"}}"#,
        ))
        .unwrap();
        assert_eq!(
            cloud,
            Some(PhaseBinding::Connection {
                connection_id: "c-1".to_string(),
                model: "m-9".to_string(),
                provider: "gemini".to_string(),
                endpoint: "https://generativelanguage.googleapis.com".to_string(),
                adapter_rev: "gemini/1".to_string(),
                effort: crate::providers::registry::ReasoningEffort::Low,
            })
        );
    }

    fn throttled_checkpoint() -> Checkpoint {
        Checkpoint::new(JobState::Scripting, 0)
    }

    fn stale_pause_display(attempt: u32) -> Option<PausedReason> {
        Some(PausedReason {
            kind: PAUSED_KIND_RATE_LIMITED.to_string(),
            class: RateLimitClass::PerMinute,
            retry_at_epoch_ms: None,
            provider: "gemini".to_string(),
            segment_idx: 0,
            last_error_message: "quota exceeded".to_string(),
            attempt,
        })
    }

    #[test]
    fn consecutive_pauses_without_progress_exhaust_at_the_cap_plus_one() {
        let mut checkpoint = throttled_checkpoint();
        assert_eq!(
            throttle_pause(&mut checkpoint, 12, 2),
            ThrottlePause::Pause(1)
        );
        assert_eq!(
            throttle_pause(&mut checkpoint, 12, 2),
            ThrottlePause::Pause(2)
        );
        assert_eq!(
            throttle_pause(&mut checkpoint, 12, 2),
            ThrottlePause::Exhausted
        );
        assert_eq!(checkpoint.rate_limit_pauses, 3);
        assert_eq!(checkpoint.last_pause_done, 12);
    }

    #[test]
    fn provider_progress_resets_the_pause_budget_and_updates_last_pause_done() {
        let mut checkpoint = throttled_checkpoint();
        assert_eq!(
            throttle_pause(&mut checkpoint, 4, 5),
            ThrottlePause::Pause(1)
        );
        assert_eq!(
            throttle_pause(&mut checkpoint, 4, 5),
            ThrottlePause::Pause(2)
        );
        assert_eq!(
            throttle_pause(&mut checkpoint, 9, 5),
            ThrottlePause::Pause(1)
        );
        assert_eq!(checkpoint.last_pause_done, 9);
        assert_eq!(checkpoint.rate_limit_pauses, 1);
    }

    #[test]
    fn a_done_regression_does_not_re_arm_the_budget() {
        let mut checkpoint = throttled_checkpoint();
        assert_eq!(
            throttle_pause(&mut checkpoint, 9, 3),
            ThrottlePause::Pause(1)
        );
        assert_eq!(
            throttle_pause(&mut checkpoint, 3, 3),
            ThrottlePause::Pause(2)
        );
        assert_eq!(
            throttle_pause(&mut checkpoint, 3, 3),
            ThrottlePause::Pause(3)
        );
        assert_eq!(
            throttle_pause(&mut checkpoint, 3, 3),
            ThrottlePause::Exhausted
        );
    }

    #[test]
    fn the_pause_display_never_feeds_the_throttle_decision() {
        let mut checkpoint = throttled_checkpoint();
        checkpoint.paused_reason = stale_pause_display(4);
        assert_eq!(
            throttle_pause(&mut checkpoint, 2, 5),
            ThrottlePause::Pause(1)
        );
        checkpoint.paused_reason = None;
        assert_eq!(
            throttle_pause(&mut checkpoint, 2, 5),
            ThrottlePause::Pause(2)
        );
        checkpoint.paused_reason = stale_pause_display(9);
        assert_eq!(
            throttle_pause(&mut checkpoint, 2, 5),
            ThrottlePause::Pause(3)
        );
    }

    #[test]
    fn waits_past_the_auto_resume_ceiling_classify_daily_and_shorter_waits_per_minute() {
        let ceiling = MAX_AUTO_RESUME_WAIT_SECS as i64;
        assert_eq!(auto_resume_class(ceiling + 1), RateLimitClass::Daily);
        assert_eq!(auto_resume_class(30_000), RateLimitClass::Daily);
        assert_eq!(auto_resume_class(ceiling), RateLimitClass::PerMinute);
        assert_eq!(auto_resume_class(0), RateLimitClass::PerMinute);
    }

    #[tokio::test]
    async fn a_keyless_record_that_is_not_loopback_refuses_at_the_request_seam() {
        // The store refuses to WRITE this shape; a hand-edited settings row
        // can still be READ back in it, so the adapter seam re-checks A1.
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        let tampered = ConnectionRecord {
            id: "c-tampered".to_string(),
            name: "Tampered".to_string(),
            provider: ProviderKind::OpenaiCompatible,
            base_url: Some("https://api.example.com/v1".to_string()),
            credential_not_required: true,
            default_model: "m".to_string(),
            enabled: true,
            created_unix: 0,
            updated_unix: 0,
        };
        let err = match build_adapter(&pool, None, &tampered, "m").await {
            Ok(_) => panic!("keyless-remote must refuse"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("loopback"),
            "keyless-remote must refuse with the A1 reason, got: {err}"
        );

        let loopback = ConnectionRecord {
            base_url: Some("http://127.0.0.1:11434/v1".to_string()),
            ..tampered
        };
        assert!(
            build_adapter(&pool, None, &loopback, "m").await.is_ok(),
            "lawful loopback keyless still builds without the vault"
        );
    }

    #[test]
    fn a_present_but_malformed_binding_refuses_instead_of_going_local() {
        for malformed in [
            r#"{"script_binding":"phi-4-mini"}"#,
            r#"{"script_binding":{"model_id":"x"}}"#,
            r#"{"script_binding":{"kind":"connection","connection_id":"c"}}"#,
            r#"{"script_binding":{"kind":"connection","connection_id":"c","model":"m"}}"#,
            r#"{"script_binding":{"kind":"local","model_id":"x","temperature":0.7}}"#,
        ] {
            let err = override_script_binding(Some(malformed)).unwrap_err();
            assert!(
                matches!(err, OrchestratorError::InvalidRequest(_)),
                "{malformed} must refuse, not silently resolve to Local"
            );
        }
    }

    use crate::orchestrator::stage::{control_channel, ControlHandle};
    use crate::orchestrator::state::{Checkpoint, JobState};
    use crate::orchestrator::ProgressEvent;
    use crate::providers::registry::{ScriptRequest, ScriptResponse, ScriptUsage};
    use crate::storage::{new_id, SqlitePool};
    use crate::voice::keys::store::Backend;
    use crate::voice::keys::{KeyManager, KeyState, KeyStore};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn test_unit(idx: i64, text: &str) -> SourceUnit {
        SourceUnit {
            chapter_idx: 0,
            page_no: 1,
            kind: "text".to_string(),
            text: text.to_string(),
            figure_id: None,
            region_type: None,
            caption: None,
            explanation: None,
            block_index: Some(idx),
            label: None,
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
        ControlHandle,
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
            mode: "teacher".into(),
            voice_profile_id: "voice-1".into(),
            output_format: "mp3".into(),
            model_overrides_json: None,
            resource_plan_json: None,
            vision_plan_json: None,
            tts_plan_json: None,
            align_plan_json: None,
            state: JobState::Scripting.as_str().into(),
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

        let (control_handle, control_rx) = control_channel();
        let progress_sink = Arc::new(|_: ProgressEvent| {});
        let ctx = StageContext::new(
            pool.clone(),
            job_id,
            JobState::Scripting,
            Checkpoint::new(JobState::Scripting, now_ms()),
            control_rx,
            progress_sink,
        );

        let connection = ConnectionRecord {
            id: "c-test".to_string(),
            name: "Google Gemini".to_string(),
            provider: ProviderKind::Gemini,
            base_url: None,
            credential_not_required: false,
            default_model: "gemini-2.5-flash".to_string(),
            enabled: true,
            created_unix: 0,
            updated_unix: 0,
        };

        (dir, pool, manager, connection, ctx, control_handle)
    }

    struct NoOpAdapterFactory;

    #[async_trait::async_trait]
    impl AdapterFactory for NoOpAdapterFactory {
        async fn rebuild(&self) -> Result<(Box<dyn ScriptLlm>, Option<String>), OrchestratorError> {
            Err(OrchestratorError::Engine {
                status: None,
                code: "ERR_NO_KEYS".to_string(),
                message: "no alternative keys available in test harness".to_string(),
            })
        }
    }

    type FactoryRebuildResult = Result<(Box<dyn ScriptLlm>, Option<String>), OrchestratorError>;

    struct RotatingAdapterFactory {
        adapters: std::sync::Mutex<Vec<FactoryRebuildResult>>,
    }

    #[async_trait::async_trait]
    impl AdapterFactory for RotatingAdapterFactory {
        async fn rebuild(&self) -> Result<(Box<dyn ScriptLlm>, Option<String>), OrchestratorError> {
            let mut list = self.adapters.lock().unwrap();
            if list.is_empty() {
                Err(OrchestratorError::Engine {
                    status: None,
                    code: "ERR_NO_KEYS".to_string(),
                    message: "exhausted test adapters".to_string(),
                })
            } else {
                list.remove(0)
            }
        }
    }

    struct MockTeacherLlm {
        calls: Arc<AtomicU32>,
        fail_on_call: Option<u32>,
        fail_non_overload: bool,
    }

    #[async_trait::async_trait]
    impl ScriptLlm for MockTeacherLlm {
        async fn generate(
            &mut self,
            _request: ScriptRequest,
        ) -> Result<ScriptResponse, ScriptError> {
            let count = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_non_overload {
                return Err(ScriptError {
                    kind: ScriptErrorKind::Provider,
                    retryable: false,
                    message: "400 Bad Request: API_KEY_INVALID".to_string(),
                    retry_after_secs: None,
                    status: Some(400),
                });
            }
            if self.fail_on_call == Some(count) {
                return Err(ScriptError {
                    kind: ScriptErrorKind::Provider,
                    retryable: true,
                    message: "503 UNAVAILABLE: The model is overloaded. Please try again later."
                        .to_string(),
                    retry_after_secs: None,
                    status: Some(503),
                });
            }
            Ok(ScriptResponse {
                text: "[{\"text\": \"This is a generated lecture paragraph.\"}]".to_string(),
                usage: ScriptUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(20),
                },
            })
        }
    }

    #[tokio::test]
    async fn overload_503_retries_on_same_key_and_succeeds_inside_patience_window() {
        let (_dir, pool, manager, connection, mut ctx, _control) = setup_test_harness(1).await;
        let mut u0 = test_unit(0, "First section");
        u0.kind = "heading".to_string();
        let mut u1 = test_unit(1, "Second section");
        u1.kind = "heading".to_string();
        let units = vec![u0, u1];
        let units_fingerprint = "test-fingerprint";
        let total_chunks = 2i64;
        let resume = Vec::new();

        let calls = Arc::new(AtomicU32::new(0));
        let mut adapter: Box<dyn ScriptLlm> = Box::new(MockTeacherLlm {
            calls: calls.clone(),
            // Call 0: chunk 0 lecture (ok)
            // Call 1: summary update (ok)
            // Call 2: chunk 1 lecture (fails 503!)
            // Call 3: chunk 1 lecture retry (ok)
            fail_on_call: Some(2),
            fail_non_overload: false,
        });

        let patience = PatienceConfig::for_test();
        let res = run_cloud_script_teacher_loop(
            &mut ctx,
            &units,
            units_fingerprint,
            total_chunks,
            resume,
            &mut adapter,
            &connection,
            Some("KEY_0".to_string()),
            Some(&manager),
            patience,
            &NoOpAdapterFactory,
        )
        .await
        .unwrap();

        match res {
            TeacherLoopOutcome::Completed {
                outcomes,
                done_chunks,
            } => {
                assert_eq!(outcomes.len(), 2);
                assert_eq!(done_chunks, 2);
            }
            other => panic!("expected Completed outcome, got {other:?}"),
        }

        // Chunk 0 was NOT re-requested on attempt 2:
        // Call 0: chunk 0 lecture
        // Call 1: summary update
        // Call 2: chunk 1 lecture (fails 503)
        // Call 3: chunk 1 lecture retry (succeeds)
        assert_eq!(calls.load(Ordering::SeqCst), 4);

        // Verify key was NOT demoted
        let keys = manager.list_entries(&pool).await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_ne!(keys[0].state, KeyState::OutOfQuota);
    }

    #[tokio::test]
    async fn rate_limit_429_retries_on_same_key_and_succeeds_inside_cooldown() {
        let (_dir, pool, manager, connection, mut ctx, _control) = setup_test_harness(1).await;
        let mut u0 = test_unit(0, "First section");
        u0.kind = "heading".to_string();
        let units = vec![u0];
        let units_fingerprint = "test-fingerprint";
        let total_chunks = 1i64;
        let resume = Vec::new();

        struct MockRateLimitThenSuccessLlm {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl ScriptLlm for MockRateLimitThenSuccessLlm {
            async fn generate(
                &mut self,
                _request: ScriptRequest,
            ) -> Result<ScriptResponse, ScriptError> {
                let count = self.calls.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    return Err(ScriptError {
                        kind: ScriptErrorKind::Quota,
                        retryable: true,
                        message: "RESOURCE_EXHAUSTED: quota exceeded".to_string(),
                        retry_after_secs: Some(1),
                        status: Some(429),
                    });
                }
                Ok(ScriptResponse {
                    text: "[{\"text\": \"This is a generated lecture paragraph.\"}]".to_string(),
                    usage: ScriptUsage {
                        input_tokens: Some(10),
                        output_tokens: Some(20),
                    },
                })
            }
        }

        let entries = manager.list_entries(&pool).await.unwrap();
        let key_account = entries[0].account.clone();

        let calls = Arc::new(AtomicU32::new(0));
        let mut adapter: Box<dyn ScriptLlm> = Box::new(MockRateLimitThenSuccessLlm {
            calls: calls.clone(),
        });

        let patience = PatienceConfig::for_test();
        let res = run_cloud_script_teacher_loop(
            &mut ctx,
            &units,
            units_fingerprint,
            total_chunks,
            resume,
            &mut adapter,
            &connection,
            Some(key_account),
            Some(&manager),
            patience,
            &NoOpAdapterFactory,
        )
        .await
        .unwrap();

        match res {
            TeacherLoopOutcome::Completed {
                outcomes,
                done_chunks,
            } => {
                assert_eq!(outcomes.len(), 1);
                assert_eq!(done_chunks, 1);
            }
            other => panic!("expected Completed outcome, got {other:?}"),
        }

        // Call 0 failed 429, Call 1 succeeded lecture (no summary for single/final chunk)
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let keys = manager.list_entries(&pool).await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].state, KeyState::Active);
    }

    #[tokio::test]
    async fn rate_limit_429_rotates_to_next_key_without_sleeping() {
        let (_dir, pool, manager, connection, mut ctx, _control) = setup_test_harness(2).await;
        let entries = manager.list_entries(&pool).await.unwrap();
        assert_eq!(entries.len(), 2);
        let key0_account = entries[0].account.clone();
        let key1_account = entries[1].account.clone();

        let mut u0 = test_unit(0, "First section");
        u0.kind = "heading".to_string();
        let units = vec![u0];
        let units_fingerprint = "test-fingerprint";
        let total_chunks = 1i64;
        let resume = Vec::new();

        struct MockFailWith429;
        #[async_trait::async_trait]
        impl ScriptLlm for MockFailWith429 {
            async fn generate(
                &mut self,
                _request: ScriptRequest,
            ) -> Result<ScriptResponse, ScriptError> {
                Err(ScriptError {
                    kind: ScriptErrorKind::Quota,
                    retryable: true,
                    message: "RESOURCE_EXHAUSTED: quota exceeded".to_string(),
                    retry_after_secs: Some(60),
                    status: Some(429),
                })
            }
        }

        struct MockSuccessLlm {
            calls: Arc<AtomicU32>,
        }
        #[async_trait::async_trait]
        impl ScriptLlm for MockSuccessLlm {
            async fn generate(
                &mut self,
                _request: ScriptRequest,
            ) -> Result<ScriptResponse, ScriptError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(ScriptResponse {
                    text:
                        "[{\"text\": \"This is a generated lecture paragraph from rotated key.\"}]"
                            .to_string(),
                    usage: ScriptUsage {
                        input_tokens: Some(10),
                        output_tokens: Some(20),
                    },
                })
            }
        }

        let key1_calls = Arc::new(AtomicU32::new(0));
        let adapter_key1: Box<dyn ScriptLlm> = Box::new(MockSuccessLlm {
            calls: key1_calls.clone(),
        });

        let factory = RotatingAdapterFactory {
            adapters: std::sync::Mutex::new(vec![Ok((adapter_key1, Some(key1_account.clone())))]),
        };

        let mut initial_adapter: Box<dyn ScriptLlm> = Box::new(MockFailWith429);
        let patience = PatienceConfig::default();

        let start_time = tokio::time::Instant::now();
        let res = run_cloud_script_teacher_loop(
            &mut ctx,
            &units,
            units_fingerprint,
            total_chunks,
            resume,
            &mut initial_adapter,
            &connection,
            Some(key0_account.clone()),
            Some(&manager),
            patience,
            &factory,
        )
        .await
        .unwrap();

        let elapsed = start_time.elapsed();
        assert!(
            elapsed.as_secs() < 2,
            "Key rotation must complete immediately without sleeping away the 60s cooldown; took {:?}",
            elapsed
        );

        match res {
            TeacherLoopOutcome::Completed {
                outcomes,
                done_chunks,
            } => {
                assert_eq!(outcomes.len(), 1);
                assert_eq!(done_chunks, 1);
            }
            other => panic!("expected Completed outcome, got {other:?}"),
        }

        assert_eq!(key1_calls.load(Ordering::SeqCst), 1);

        // Verify key states in KeyManager:
        // Key 0 must be demoted to OutOfQuota
        // Key 1 must be Active (since record_pool_outcome succeeded on it)
        let entries = manager.list_entries(&pool).await.unwrap();
        let k0 = entries.iter().find(|k| k.account == key0_account).unwrap();
        let k1 = entries.iter().find(|k| k.account == key1_account).unwrap();
        assert_eq!(k0.state, KeyState::OutOfQuota);
        assert_eq!(k1.state, KeyState::Active);
    }

    #[tokio::test]
    async fn overload_503_does_not_rotate_to_next_key() {
        let (_dir, pool, manager, connection, mut ctx, _control) = setup_test_harness(2).await;
        let entries = manager.list_entries(&pool).await.unwrap();
        assert_eq!(entries.len(), 2);
        let key0_account = entries[0].account.clone();
        let key1_account = entries[1].account.clone();

        let mut u0 = test_unit(0, "First section");
        u0.kind = "heading".to_string();
        let units = vec![u0];
        let units_fingerprint = "test-fingerprint";
        let total_chunks = 1i64;
        let resume = Vec::new();

        struct MockFailWith503;
        #[async_trait::async_trait]
        impl ScriptLlm for MockFailWith503 {
            async fn generate(
                &mut self,
                _request: ScriptRequest,
            ) -> Result<ScriptResponse, ScriptError> {
                Err(ScriptError {
                    kind: ScriptErrorKind::Provider,
                    retryable: true,
                    message: "503 UNAVAILABLE: The model is overloaded. Please try again later."
                        .to_string(),
                    retry_after_secs: None,
                    status: Some(503),
                })
            }
        }

        struct MockSuccessLlm {
            calls: Arc<AtomicU32>,
        }
        #[async_trait::async_trait]
        impl ScriptLlm for MockSuccessLlm {
            async fn generate(
                &mut self,
                _request: ScriptRequest,
            ) -> Result<ScriptResponse, ScriptError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(ScriptResponse {
                    text:
                        "[{\"text\": \"This is a generated lecture paragraph from rotated key.\"}]"
                            .to_string(),
                    usage: ScriptUsage {
                        input_tokens: Some(10),
                        output_tokens: Some(20),
                    },
                })
            }
        }

        let key1_calls = Arc::new(AtomicU32::new(0));
        let adapter_key1: Box<dyn ScriptLlm> = Box::new(MockSuccessLlm {
            calls: key1_calls.clone(),
        });

        let factory = RotatingAdapterFactory {
            adapters: std::sync::Mutex::new(vec![Ok((adapter_key1, Some(key1_account.clone())))]),
        };

        let mut initial_adapter: Box<dyn ScriptLlm> = Box::new(MockFailWith503);
        let patience = PatienceConfig {
            backoff_delays_secs: vec![0],
            max_window_secs: 0,
            pause_wait_secs: 300,
        };

        let res = run_cloud_script_teacher_loop(
            &mut ctx,
            &units,
            units_fingerprint,
            total_chunks,
            resume,
            &mut initial_adapter,
            &connection,
            Some(key0_account.clone()),
            Some(&manager),
            patience,
            &factory,
        )
        .await
        .unwrap();

        match res {
            TeacherLoopOutcome::RateLimited(reason) => {
                assert_eq!(reason.kind, PAUSED_KIND_SERVICE_BUSY);
            }
            other => panic!("expected RateLimited outcome with SERVICE_BUSY, got {other:?}"),
        }

        // Key 1 must NEVER have been called because 503 is cluster overload, not key quota
        assert_eq!(key1_calls.load(Ordering::SeqCst), 0);

        // Verify key states in KeyManager: neither key should be demoted to OutOfQuota
        let entries = manager.list_entries(&pool).await.unwrap();
        let k0 = entries.iter().find(|k| k.account == key0_account).unwrap();
        let k1 = entries.iter().find(|k| k.account == key1_account).unwrap();
        assert_ne!(k0.state, KeyState::OutOfQuota);
        assert_ne!(k1.state, KeyState::OutOfQuota);
    }

    struct MockAlwaysOverloadLlm;

    #[async_trait::async_trait]
    impl ScriptLlm for MockAlwaysOverloadLlm {
        async fn generate(
            &mut self,
            _request: ScriptRequest,
        ) -> Result<ScriptResponse, ScriptError> {
            Err(ScriptError {
                kind: ScriptErrorKind::Provider,
                retryable: true,
                message: "503 UNAVAILABLE: The model is overloaded. Please try again later."
                    .to_string(),
                retry_after_secs: None,
                status: Some(503),
            })
        }
    }

    #[tokio::test]
    async fn overload_503_persisting_past_patience_window_pauses_with_service_busy() {
        let (_dir, pool, manager, connection, mut ctx, _control) = setup_test_harness(1).await;
        let units = vec![test_unit(0, "test content")];
        let units_fingerprint = "test-fingerprint";
        let total_chunks = 1i64;
        let resume = Vec::new();

        let mut adapter: Box<dyn ScriptLlm> = Box::new(MockAlwaysOverloadLlm);

        let patience = PatienceConfig {
            backoff_delays_secs: vec![0],
            max_window_secs: 0,
            pause_wait_secs: 300,
        };
        let res = run_cloud_script_teacher_loop(
            &mut ctx,
            &units,
            units_fingerprint,
            total_chunks,
            resume,
            &mut adapter,
            &connection,
            Some("KEY_0".to_string()),
            Some(&manager),
            patience,
            &NoOpAdapterFactory,
        )
        .await
        .unwrap();

        match res {
            TeacherLoopOutcome::RateLimited(reason) => {
                assert_eq!(reason.kind, PAUSED_KIND_SERVICE_BUSY);
                assert_eq!(reason.class, RateLimitClass::PerMinute);
                assert!(reason.retry_at_epoch_ms.is_some());
                assert_eq!(reason.provider, "gemini");
                assert!(
                    reason.last_error_message.contains("temporarily overloaded")
                        || reason.last_error_message.contains("busy"),
                    "expected friendly overload message, got: {}",
                    reason.last_error_message
                );
            }
            other => panic!("expected RateLimited outcome, got {other:?}"),
        }

        // Key must remain healthy, not demoted to OutOfQuota
        let keys = manager.list_entries(&pool).await.unwrap();
        assert_eq!(keys.len(), 1);
        assert_ne!(keys[0].state, KeyState::OutOfQuota);
    }

    #[tokio::test]
    async fn non_overload_error_fails_cleanly_and_sanitizes_message() {
        let (_dir, _pool, manager, connection, mut ctx, _control) = setup_test_harness(1).await;
        let units = vec![test_unit(0, "test content")];
        let units_fingerprint = "test-fingerprint";
        let total_chunks = 1i64;
        let resume = Vec::new();

        let calls = Arc::new(AtomicU32::new(0));
        let mut adapter: Box<dyn ScriptLlm> = Box::new(MockTeacherLlm {
            calls: calls.clone(),
            fail_on_call: None,
            fail_non_overload: true,
        });

        let patience = PatienceConfig::for_test();
        let res = run_cloud_script_teacher_loop(
            &mut ctx,
            &units,
            units_fingerprint,
            total_chunks,
            resume,
            &mut adapter,
            &connection,
            Some("KEY_0".to_string()),
            Some(&manager),
            patience,
            &NoOpAdapterFactory,
        )
        .await;

        match res {
            Err(OrchestratorError::Engine { code, message, .. }) => {
                assert_eq!(code, "ERR_PROVIDER");
                assert!(
                    message.contains("Google Gemini"),
                    "message must contain provider name: {message}"
                );
                assert!(
                    !message.contains('{') && !message.contains('}'),
                    "message must not leak raw JSON: {message}"
                );
            }
            other => panic!("expected OrchestratorError::Engine, got {other:?}"),
        }
    }

    #[test]
    fn enforce_snapshot_validates_and_ignores_trailing_slash() {
        let conn = ConnectionRecord {
            id: "c1".to_string(),
            name: "Local".to_string(),
            provider: ProviderKind::OpenaiCompatible,
            base_url: Some("http://127.0.0.1:11434/v1".to_string()),
            credential_not_required: true,
            default_model: "m".to_string(),
            enabled: true,
            created_unix: 0,
            updated_unix: 0,
        };
        let rev = conn.provider.adapter_revision();

        // Exact match
        assert!(
            enforce_snapshot(&conn, "openai_compatible", "http://127.0.0.1:11434/v1", rev).is_ok()
        );

        // Trailing slash difference in endpoint must pass
        assert!(enforce_snapshot(
            &conn,
            "openai_compatible",
            "http://127.0.0.1:11434/v1/",
            rev
        )
        .is_ok());

        // Provider mismatch must fail
        assert!(enforce_snapshot(&conn, "gemini", "http://127.0.0.1:11434/v1", rev).is_err());

        // Endpoint mismatch must fail
        assert!(
            enforce_snapshot(&conn, "openai_compatible", "http://127.0.0.1:8000/v1", rev).is_err()
        );

        // Adapter rev mismatch must fail
        assert!(enforce_snapshot(
            &conn,
            "openai_compatible",
            "http://127.0.0.1:11434/v1",
            "rev-ancient"
        )
        .is_err());
    }
}
