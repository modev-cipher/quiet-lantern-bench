use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

use super::residency_check;
use super::scripting::{
    await_connection, clamp_plan_to_capability, request_run_id, ConnectionWait, SidecarProvider,
    SIDECAR_CONNECT_TIMEOUT,
};
use super::stage::{Stage, StageContext, StageOutcome};
use super::state::{JobState, OrchestratorError};
use super::stream::{cancel_run, AbortOnDrop, SidecarTarget};
use crate::catalog::bundled_catalog;
use crate::hardware;
use crate::render::mp3::cache_wav_path;
use crate::sidecar::client::{post_sse, SseEvent};
use crate::storage::{book, cache, job, models, voice};
use crate::words::{Timebase, WordsJson};

pub struct SynthesizingStage {
    client: reqwest::Client,
    sidecar: Arc<dyn SidecarProvider>,
    models_root: PathBuf,
    cache_dir: PathBuf,
    /// VA3 key custody for cloud TTS bindings (S-3). `None` (tests, minimal
    /// harnesses) makes any keyed cloud binding refuse with the reason.
    keys: Option<Arc<crate::voice::keys::KeyManager>>,
}

impl SynthesizingStage {
    pub fn new(
        sidecar: Arc<dyn SidecarProvider>,
        models_root: PathBuf,
        cache_dir: PathBuf,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(SIDECAR_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();
        SynthesizingStage {
            client,
            sidecar,
            models_root,
            cache_dir,
            keys: None,
        }
    }

    pub fn with_key_manager(mut self, keys: Arc<crate::voice::keys::KeyManager>) -> Self {
        self.keys = Some(keys);
        self
    }
}

pub const PROGRESS_FLUSH_SEGMENTS: i64 = 25;
pub const PROGRESS_FLUSH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Deserialize)]
struct RawResult {
    idx: i64,
    audio_hash: Option<String>,
    #[serde(default)]
    wav_bytes: i64,
    #[serde(default)]
    duration_ms: i64,
    #[serde(default)]
    cache_hit: bool,
    #[serde(default)]
    words: Vec<serde_json::Value>,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    words_timebase: Timebase,
}

#[derive(Default)]
struct RtfSampler {
    synth_wall_s: f64,
    audio_s: f64,
    last_result_at: Option<Instant>,
}

impl RtfSampler {
    fn start(&mut self, now: Instant) {
        self.last_result_at = Some(now);
    }

    fn record(&mut self, result: &RawResult, now: Instant) {
        let elapsed = self
            .last_result_at
            .map_or(0.0, |prev| now.duration_since(prev).as_secs_f64());
        self.last_result_at = Some(now);
        if result.cache_hit || result.audio_hash.is_none() || result.duration_ms <= 0 {
            return;
        }
        self.synth_wall_s += elapsed;
        self.audio_s += result.duration_ms as f64 / 1000.0;
    }

    fn observed_rtf(&self) -> Option<f64> {
        (self.audio_s > 0.0 && self.synth_wall_s > 0.0).then(|| self.synth_wall_s / self.audio_s)
    }
}

impl SynthesizingStage {
    async fn stage_plan(
        &self,
        ctx: &StageContext,
        record: &job::RenderJob,
        model_id: &str,
    ) -> Result<serde_json::Value, OrchestratorError> {
        if let Some(stored) = super::scripting::stored_plan_for(
            record.tts_plan_json.as_deref(),
            model_id,
            &ctx.job_id,
            "tts",
        )? {
            if stored.upcast {
                let rewritten = job::rewrite_plan_if_unchanged(
                    &ctx.pool,
                    &ctx.job_id,
                    job::PlanColumn::Tts,
                    record.tts_plan_json.as_deref().unwrap_or_default(),
                    &stored.plan.to_string(),
                )
                .await?;
                if !rewritten {
                    tracing::info!(
                        "job {} kept a concurrent tts plan write instead of its upcast",
                        ctx.job_id
                    );
                }
            }
            return Ok(stored.plan);
        }
        let catalog = bundled_catalog();
        let Some(entry) = catalog.models.iter().find(|e| e.id == model_id) else {
            return Ok(json!({
                "model_id": model_id,
                "plan_revision": crate::hardware::device::PLAN_REVISION,
                "device": crate::hardware::device::DevicePlan::cpu(),
            }));
        };
        let profile = hardware::detect(
            &self.models_root,
            hardware::DirOrigin::Default,
            &self.models_root,
        )
        .await;
        let plan = serde_json::to_value(hardware::plan::tts_fit(entry, &profile))?;
        job::set_job_tts_plan(&ctx.pool, &ctx.job_id, &plan.to_string()).await?;
        residency_check::log_stage_plan(&ctx.job_id, JobState::Synthesizing, &plan).await;
        Ok(plan)
    }
}

impl Stage for SynthesizingStage {
    fn run<'a>(
        &'a self,
        ctx: &'a mut StageContext,
    ) -> BoxFuture<'a, Result<StageOutcome, OrchestratorError>> {
        Box::pin(async move {
            let stage_started = std::time::Instant::now();
            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }

            let record = job::get_job(&ctx.pool, &ctx.job_id)
                .await?
                .ok_or_else(|| OrchestratorError::JobNotFound(ctx.job_id.clone()))?;
            // Hermes R2 blocker 4: the COMPLETE binding is parsed (and a
            // malformed one refuses) BEFORE the mutable profile row is
            // touched and BEFORE the all-audio early return — replay
            // depends on the recorded binding, never on current profile
            // state. Attribution policy: `voice_profile_id` remains a
            // required FK for history/usage naming (deletion is blocked by
            // the job-reference guard), but the CLOUD execution path below
            // never reads the profile row.
            let binding = super::synthesis_cloud::override_tts_binding(
                record.model_overrides_json.as_deref(),
            )?;

            let segments = job::list_segments(&ctx.pool, &ctx.job_id).await?;
            if segments.is_empty() {
                return Err(OrchestratorError::InvalidRequest(
                    "job has no segments to synthesize".into(),
                ));
            }

            let total = segments.len() as i64;
            let mut pending: Vec<&job::ScriptSegment> = Vec::new();
            let mut missing_ids: Vec<&str> = Vec::new();

            for segment in &segments {
                match &segment.audio_hash {
                    Some(hash)
                        if crate::render::mp3::is_valid_wav_artifact(&self.cache_dir, hash) =>
                    {
                        // Physically verified on disk
                    }
                    Some(_) => {
                        missing_ids.push(segment.id.as_str());
                        pending.push(segment);
                    }
                    None => {
                        pending.push(segment);
                    }
                }
            }

            if !missing_ids.is_empty() {
                tracing::warn!(
                    job = %ctx.job_id,
                    count = missing_ids.len(),
                    "reconciled missing audio artifacts on disk; clearing DB cache refs and re-enqueuing for synthesis"
                );
                job::clear_segments_audio(&ctx.pool, &missing_ids).await?;
                job::delete_chapter_renders_for_job(&ctx.pool, &ctx.job_id).await?;
            }

            let done_before = total - pending.len() as i64;
            ctx.checkpoint.total_segments = total;
            ctx.checkpoint.done_segments = done_before;
            ctx.save_checkpoint().await?;
            if pending.is_empty() {
                ctx.emit_progress(1.0, Some(0), "tts cached");
                ctx.merge_report_duration("tts", stage_started.elapsed().as_secs())
                    .await?;
                return Ok(StageOutcome::Completed);
            }

            if let Some(cloud_binding @ crate::providers::TtsBinding::Connection { .. }) = &binding
            {
                return self
                    .run_cloud(
                        ctx,
                        cloud_binding,
                        &record,
                        &pending,
                        total,
                        done_before,
                        stage_started,
                    )
                    .await;
            }
            ctx.emit_progress(
                done_before as f64 / total as f64,
                None,
                "loading the voice model",
            );
            // The LOCAL path is the only reader of the mutable profile row.
            let profile = voice::get_voice_profile(&ctx.pool, &record.voice_profile_id)
                .await?
                .ok_or_else(|| {
                    OrchestratorError::VoiceProfileNotFound(record.voice_profile_id.clone())
                })?;
            // A recorded structural Local binding names the local TTS model
            // verbatim (A15: the binding is the recorded truth); absent one,
            // the profile's engine remains the selector it always was.
            let model_id = match &binding {
                Some(crate::providers::TtsBinding::Local { model_id }) => model_id.clone(),
                _ => profile.engine.clone(),
            };

            let book = book::get_book(&ctx.pool, &record.book_id).await?;
            let book_language = book
                .as_ref()
                .and_then(|b| b.language.as_deref())
                .and_then(crate::voices::normalize_lang);
            let engine_languages = bundled_catalog()
                .models
                .iter()
                .find(|e| e.id == model_id)
                .map(|e| e.languages.as_slice())
                .unwrap_or(&[]);
            if let Some(mismatch) = crate::voices::language_advisory(
                book_language.as_deref(),
                profile.preset_voice_id.as_deref(),
                engine_languages,
            ) {
                match mismatch.voice_speaks.as_deref() {
                    Some(spoken) => tracing::warn!(
                        "job {}: the selected voice speaks '{spoken}' but this book is \
                         in '{}'; casting anyway",
                        ctx.job_id,
                        mismatch.book
                    ),
                    None => tracing::warn!(
                        "job {}: the selected voice's engine does not list this book's \
                         language '{}'; casting anyway",
                        ctx.job_id,
                        mismatch.book
                    ),
                }
                ctx.merge_report_value(
                    "language_advisory",
                    json!({"book": mismatch.book, "voice_speaks": mismatch.voice_speaks}),
                )
                .await?;
            }

            let install = models::get_model_install(&ctx.pool, &model_id)
                .await?
                .ok_or_else(|| OrchestratorError::ModelNotInstalled(model_id.clone()))?;
            let model_dir = self.models_root.join(&install.path);

            let (port, token) = match await_connection(&*self.sidecar, &ctx.control).await? {
                ConnectionWait::Ready(port, token) => (port, token),
                ConnectionWait::Interrupted(outcome) => return Ok(outcome),
            };

            let tts_seed = match record.tts_seed {
                Some(seed) => seed,
                None => {
                    let book = book
                        .as_ref()
                        .ok_or_else(|| OrchestratorError::BookNotFound(record.book_id.clone()))?;
                    job::default_tts_seed(&record.voice_profile_id, &book.content_hash)
                }
            };

            let voice_params: serde_json::Value = profile
                .params_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or(serde_json::Value::Null);
            let resource_plan = self.stage_plan(ctx, &record, &model_id).await?;
            let resource_plan = clamp_plan_to_capability(
                &self.client,
                port,
                &token,
                resource_plan,
                "torch_cuda",
                false,
            )
            .await;

            let body = json!({
                "segments": pending
                    .iter()
                    .map(|s| json!({"idx": s.idx, "text": s.text}))
                    .collect::<Vec<_>>(),
                "voice_profile": {
                    "id": profile.id,
                    "kind": profile.kind,
                    "preset_voice_id": profile.preset_voice_id,
                    "reference_wav_path": profile.reference_wav_path,
                    "params": voice_params,
                    "seed": tts_seed,
                },
                "model_id": model_id,
                "model_dir": model_dir.to_string_lossy(),
                "cache_dir": self.cache_dir.to_string_lossy(),
                "resource_plan": resource_plan,
                "stage": JobState::Synthesizing.as_str(),
                "language": book_language,
            });

            let pending_total = pending.len() as i64;
            let (tx, mut rx) = mpsc::unbounded_channel::<SseEvent>();
            let client = self.client.clone();
            let run_id = request_run_id(&ctx.job_id);
            let cancel_token = token.clone();
            let cancel_run_id = run_id.clone();
            let mut sse_task = AbortOnDrop(tokio::spawn(async move {
                post_sse(&client, port, &token, "/tts", &run_id, &body, |event| {
                    let _ = tx.send(event);
                })
                .await
            }));

            let device = crate::hardware::device::plan_device(&resource_plan)
                .unwrap_or_else(|_| crate::hardware::device::DevicePlan::cpu())
                .learned_key();

            let started = Instant::now();
            let mut sampler = RtfSampler::default();
            sampler.start(started);
            let mut control = ctx.control.clone();
            let mut done = done_before;
            let mut last_flushed = done_before;
            let mut last_flush_at = Instant::now();
            let mut last_label = "segment synthesized";
            let mut cancelled = false;
            let mut finished = false;
            let mut stage_flags: Vec<String> = Vec::new();
            let mut sidecar_error: Option<OrchestratorError> = None;
            let mut parse_error: Option<String> = None;
            loop {
                let event = tokio::select! {
                    outcome = control.interrupted() => {
                        cancel_run(
                            SidecarTarget::new(&self.client, port, &cancel_token),
                            &cancel_run_id,
                        )
                        .await;
                        flush_progress(
                            ctx, done, &mut last_flushed, total, done_before,
                            pending_total, started, last_label,
                        )
                        .await?;
                        return Ok(outcome);
                    }
                    event = rx.recv() => event,
                };
                let Some(event) = event else {
                    break;
                };
                match event.event.as_str() {
                    "result" => {
                        let raw = event
                            .data
                            .get("result")
                            .cloned()
                            .ok_or_else(|| "result event missing payload".to_string())
                            .and_then(|value| {
                                serde_json::from_value::<RawResult>(value)
                                    .map_err(|err| err.to_string())
                            });
                        let result = match raw {
                            Ok(result) => result,
                            Err(err) => {
                                parse_error = Some(err);
                                continue;
                            }
                        };
                        let Some(segment) = segments.iter().find(|s| s.idx == result.idx) else {
                            tracing::warn!(
                                "tts result for unknown segment idx {}, skipping",
                                result.idx
                            );
                            continue;
                        };
                        sampler.record(&result, Instant::now());
                        let succeeded =
                            apply_result(ctx, segment, &result, &self.cache_dir).await?;
                        done += 1;
                        last_label = if succeeded {
                            "segment synthesized"
                        } else {
                            "segment failed"
                        };
                        if done - last_flushed >= PROGRESS_FLUSH_SEGMENTS
                            || last_flush_at.elapsed() >= PROGRESS_FLUSH_INTERVAL
                        {
                            flush_progress(
                                ctx,
                                done,
                                &mut last_flushed,
                                total,
                                done_before,
                                pending_total,
                                started,
                                last_label,
                            )
                            .await?;
                            last_flush_at = Instant::now();
                        }
                    }
                    "cancelled" => cancelled = true,
                    "error" => {
                        sidecar_error = Some(OrchestratorError::from_engine_event(
                            &event.data,
                            "tts failed",
                        ));
                    }
                    "done" => {
                        if let Some(value) = event.data.get("flags") {
                            stage_flags = serde_json::from_value::<Vec<String>>(value.clone())
                                .unwrap_or_default();
                        }
                        finished = true;
                    }
                    _ => {}
                }
            }

            flush_progress(
                ctx,
                done,
                &mut last_flushed,
                total,
                done_before,
                pending_total,
                started,
                last_label,
            )
            .await?;

            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }
            if cancelled {
                return Ok(StageOutcome::Cancelled);
            }
            if let Some(error) = sidecar_error {
                return Err(error);
            }
            if let Some(err) = parse_error {
                return Err(OrchestratorError::Sidecar(format!(
                    "failed to parse tts result: {err}"
                )));
            }
            ctx.merge_report_flags("tts_flags", &stage_flags).await?;
            match (&mut sse_task.0).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => return Err(OrchestratorError::from(err)),
                Err(err) => {
                    return Err(OrchestratorError::Sidecar(format!(
                        "tts stream task failed: {err}"
                    )))
                }
            }
            if !finished {
                return Err(OrchestratorError::Sidecar(
                    "tts stream ended without completion".into(),
                ));
            }

            if let Some(rtf) = sampler.observed_rtf() {
                let model_id = model_id.clone();
                if let Err(err) = models::update_learned(&ctx.pool, |c| {
                    c.record_tts_rtf(&model_id, &device, rtf);
                })
                .await
                {
                    tracing::warn!("failed to persist learned tts rtf: {err}");
                }
            }

            ctx.emit_progress(
                1.0,
                Some(0),
                &format!("{done}/{total} segments synthesized"),
            );
            ctx.merge_report_duration("tts", stage_started.elapsed().as_secs())
                .await?;
            Ok(StageOutcome::Completed)
        })
    }
}

impl SynthesizingStage {
    /// The S-3 cloud TTS wired path: per pending segment, synthesize over
    /// the recorded binding's adapter, land the wav in the ordinary cache,
    /// admit native timing only past the whole-set gate (A21), and honor
    /// pause/cancel between segments. No sidecar, no local model — the
    /// align phase later covers whatever has no native timing.
    #[allow(clippy::too_many_arguments)]
    async fn run_cloud(
        &self,
        ctx: &mut StageContext,
        binding: &crate::providers::TtsBinding,
        record: &job::RenderJob,
        pending: &[&job::ScriptSegment],
        total: i64,
        done_before: i64,
        stage_started: Instant,
    ) -> Result<StageOutcome, OrchestratorError> {
        let crate::providers::TtsBinding::Connection {
            connection_id,
            model,
            voice,
            provider,
            endpoint,
            adapter_rev,
        } = binding
        else {
            return Err(OrchestratorError::InvalidRequest(
                "run_cloud requires a connection tts binding".to_string(),
            ));
        };
        // T18 ordering (Hermes R2 fold-in 12): the snapshot is enforced on
        // the resolved connection BEFORE any key custody lookup or adapter
        // construction — a rebound endpoint is refused before the wrong
        // pool is ever touched.
        let connection = super::scripting_cloud::resolve_connection(
            &ctx.pool,
            connection_id,
            crate::providers::PhaseCapability::TtsSynth,
        )
        .await?;
        super::scripting_cloud::enforce_narration_snapshot(
            &connection,
            provider,
            endpoint,
            adapter_rev,
            model,
        )?;
        ctx.emit_progress(
            done_before as f64 / total as f64,
            None,
            &format!(
                "voicing with {}",
                super::overload::connection_label(&connection)
            ),
        );
        let mut plan = match super::synthesis_cloud::prepare_cloud_tts(
            &ctx.pool,
            self.keys.as_ref(),
            connection,
            model,
            &self.cache_dir,
            Some(ctx),
        )
        .await?
        {
            super::synthesis_cloud::PreparedCloudTts::Ready(plan) => plan,
            super::synthesis_cloud::PreparedCloudTts::Paused(reason) => {
                tracing::warn!(
                    job = %ctx.job_id,
                    reason = ?reason,
                    "synthesis re-entering while pool is in quota cooldown; re-pausing honestly"
                );
                ctx.checkpoint.paused_reason = Some(reason.clone());
                ctx.save_checkpoint().await?;
                let _ = ctx
                    .merge_report_value(
                        "paused_reason",
                        serde_json::to_value(&reason).unwrap_or_default(),
                    )
                    .await;
                return Ok(StageOutcome::Paused);
            }
        };
        tracing::info!(
            job = %ctx.job_id,
            connection = %plan.connection.id,
            provider = plan.connection.provider.id(),
            model,
            "stage SYNTHESIZING running cloud tts over the recorded binding"
        );
        // A24: the report names the recorded tuple, never a re-resolved one.
        ctx.merge_report_value(
            "tts_provider",
            json!({
                "connection_id": plan.connection.id,
                "connection_name": plan.connection.name,
                "provider": plan.connection.provider.id(),
                "model": model,
                "voice": voice,
            }),
        )
        .await?;

        // A22: a timestamp-less provider with no align model installed means
        // read-along timing will degrade honestly; say so up front.
        let book = book::get_book(&ctx.pool, &record.book_id).await?;
        let book_language = book.as_ref().and_then(|b| b.language.as_deref());
        let installs = models::list_model_installs(&ctx.pool).await?;
        let align_setting =
            crate::storage::settings::get_setting(&ctx.pool, super::align::DEFAULT_ALIGN_SETTING)
                .await?;
        let align_resolved = super::align::resolve_align_id(
            record.model_overrides_json.as_deref(),
            align_setting.as_deref(),
            bundled_catalog(),
            &installs,
            &self.models_root,
            book_language,
        )
        .is_some();
        if super::synthesis_cloud::align_advisory_needed(plan.connection.provider, align_resolved) {
            tracing::warn!(
                job = %ctx.job_id,
                "cloud tts provider has no native timing and no align model is installed; \
                 read-along highlighting will degrade to sentence fallback"
            );
            ctx.merge_report_value(
                "align_advisory",
                json!({"reason": "timestampless_provider_without_align_model"}),
            )
            .await?;
        }

        let pending_total = pending.len() as i64;
        let started = Instant::now();
        let mut done = done_before;
        let mut last_flushed = done_before;
        let mut native_segments: i64 = 0;
        let mut fallback_segments: i64 = 0;
        let mut pause_report_stripped = false;
        let request_identity = |text: &str| {
            super::synthesis_cloud::request_cache_key(
                provider,
                endpoint,
                model,
                voice,
                text,
                adapter_rev,
            )
        };
        for segment in pending {
            if let Some(outcome) = super::synthesis_cloud::interruption(ctx) {
                flush_progress(
                    ctx,
                    done,
                    &mut last_flushed,
                    total,
                    done_before,
                    pending_total,
                    started,
                    "segment synthesized",
                )
                .await?;
                return Ok(outcome);
            }
            // MF6: an identical request (same provider/endpoint/model/voice/
            // text/adapter revision) whose audio is still cached is answered
            // from the cache — a paid request is never repeated.
            let request_key = request_identity(&segment.text);
            if let Some(hit) = super::synthesis_cloud::replay_cached_request(
                ctx,
                &request_key,
                segment,
                &self.cache_dir,
            )
            .await?
            {
                if hit.native_timing {
                    native_segments += 1;
                } else {
                    fallback_segments += 1;
                }
                done += 1;
                flush_progress(
                    ctx,
                    done,
                    &mut last_flushed,
                    total,
                    done_before,
                    pending_total,
                    started,
                    "segment synthesized",
                )
                .await?;
                continue;
            }
            let run = super::synthesis_cloud::synthesize_segment_cancellable(
                ctx,
                &mut plan,
                segment,
                voice,
                &self.cache_dir,
                &request_key,
                self.keys.as_deref(),
            )
            .await?;
            let outcome = match run {
                super::synthesis_cloud::SegmentRun::Done(outcome) => {
                    if ctx.checkpoint.paused_reason.take().is_some() {
                        ctx.save_checkpoint().await?;
                    }
                    if !pause_report_stripped {
                        pause_report_stripped = true;
                        if let Err(error) = super::synthesis_cloud::strip_report_paused_reason(
                            &ctx.pool,
                            &ctx.job_id,
                        )
                        .await
                        {
                            tracing::warn!(
                                job = %ctx.job_id,
                                "failed to clear the persisted pause reason: {error}"
                            );
                        }
                    }
                    outcome
                }
                super::synthesis_cloud::SegmentRun::Interrupted(outcome) => {
                    flush_progress(
                        ctx,
                        done,
                        &mut last_flushed,
                        total,
                        done_before,
                        pending_total,
                        started,
                        "segment synthesized",
                    )
                    .await?;
                    return Ok(outcome);
                }
                super::synthesis_cloud::SegmentRun::RateLimited(reason) => {
                    flush_progress(
                        ctx,
                        done,
                        &mut last_flushed,
                        total,
                        done_before,
                        pending_total,
                        started,
                        "segment rate limited",
                    )
                    .await?;
                    ctx.checkpoint.paused_reason = Some(reason.clone());
                    ctx.save_checkpoint().await?;
                    let _ = ctx
                        .merge_report_value(
                            "paused_reason",
                            serde_json::to_value(&reason).unwrap_or_default(),
                        )
                        .await;
                    return Ok(StageOutcome::Paused);
                }
            };
            if outcome.native_timing {
                native_segments += 1;
            } else {
                fallback_segments += 1;
            }
            done += 1;
            flush_progress(
                ctx,
                done,
                &mut last_flushed,
                total,
                done_before,
                pending_total,
                started,
                "segment synthesized",
            )
            .await?;

            // Rotate round-robin across pool for next segment so that requests are
            // distributed evenly across all keys in the pool.
            let _ = plan.rotate_pool(&ctx.pool, self.keys.as_deref()).await;
        }

        ctx.merge_report_value(
            "tts_native_timing",
            json!({"native_segments": native_segments, "align_fallback_segments": fallback_segments}),
        )
        .await?;
        ctx.emit_progress(
            1.0,
            Some(0),
            &format!("{done}/{total} segments synthesized"),
        );
        ctx.merge_report_duration("tts", stage_started.elapsed().as_secs())
            .await?;
        Ok(StageOutcome::Completed)
    }
}

#[allow(clippy::too_many_arguments)]
async fn flush_progress(
    ctx: &mut StageContext,
    done: i64,
    last_flushed: &mut i64,
    total: i64,
    done_before: i64,
    pending_total: i64,
    started: Instant,
    label: &str,
) -> Result<(), OrchestratorError> {
    if done == *last_flushed {
        return Ok(());
    }
    ctx.checkpoint.done_segments = done;
    ctx.save_checkpoint().await?;
    *last_flushed = done;
    let synth_done = done - done_before;
    let eta_seconds = if synth_done > 0 && pending_total > synth_done {
        let rate = started.elapsed().as_secs_f64() / synth_done as f64;
        Some((rate * (pending_total - synth_done) as f64) as i64)
    } else {
        None
    };
    ctx.emit_progress(done as f64 / total as f64, eta_seconds, label);
    Ok(())
}

pub const NATIVE_WORDS_MODEL_ID: &str = "tts_native";

async fn claim_native_words(
    ctx: &StageContext,
    audio_hash: &str,
    cache_dir: &Path,
) -> Result<(), OrchestratorError> {
    let key = super::align::align_cache_key(audio_hash, NATIVE_WORDS_MODEL_ID);
    let Ok(meta) = std::fs::metadata(cache_wav_path(cache_dir, &key)) else {
        return Ok(());
    };
    cache::record_use(
        &ctx.pool,
        &key,
        "align",
        meta.len() as i64,
        Some(&ctx.job_id),
    )
    .await?;
    Ok(())
}

async fn apply_result(
    ctx: &StageContext,
    segment: &job::ScriptSegment,
    result: &RawResult,
    cache_dir: &Path,
) -> Result<bool, OrchestratorError> {
    if !result.flags.is_empty() {
        let mut flags: Vec<String> = segment
            .flags_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        for flag in &result.flags {
            if !flags.contains(flag) {
                flags.push(flag.clone());
            }
        }
        job::set_segment_flags(&ctx.pool, &segment.id, &serde_json::to_string(&flags)?).await?;
    }
    let Some(audio_hash) = result.audio_hash.as_deref() else {
        return Ok(false);
    };
    let words_json = if !result.words.is_empty() {
        Some(WordsJson::declare(&result.words, result.words_timebase)?)
    } else {
        None
    };
    job::settle_local_segment(
        &ctx.pool,
        &ctx.job_id,
        &segment.id,
        audio_hash,
        result.wav_bytes,
        result.duration_ms,
        words_json.as_ref().map(|w| w.as_str()),
    )
    .await?;
    if words_json.is_some() {
        claim_native_words(ctx, audio_hash, cache_dir).await?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(cache_hit: bool, duration_ms: i64, hash: Option<&str>) -> RawResult {
        RawResult {
            idx: 0,
            audio_hash: hash.map(str::to_string),
            wav_bytes: 0,
            duration_ms,
            cache_hit,
            words: vec![],
            flags: vec![],
            words_timebase: Timebase::Unknown,
        }
    }

    #[test]
    fn sampler_computes_rtf_from_wall_over_audio() {
        let mut sampler = RtfSampler::default();
        let t0 = Instant::now();
        sampler.start(t0);
        sampler.record(
            &result(false, 1000, Some("a")),
            t0 + Duration::from_millis(500),
        );
        let rtf = sampler.observed_rtf().expect("rtf");
        assert!((rtf - 0.5).abs() < 0.05, "rtf {rtf}");
    }

    #[test]
    fn sampler_ignores_cache_hits_and_failures() {
        let mut sampler = RtfSampler::default();
        let t0 = Instant::now();
        sampler.start(t0);
        sampler.record(
            &result(true, 1000, Some("a")),
            t0 + Duration::from_millis(500),
        );
        sampler.record(&result(false, 0, None), t0 + Duration::from_millis(600));
        assert!(sampler.observed_rtf().is_none());
    }
}
