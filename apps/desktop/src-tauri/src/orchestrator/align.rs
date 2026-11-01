use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use super::contention::{classify_code, StageFailure};
use super::residency_check;
use super::scripting::{
    await_connection, request_run_id, ConnectionWait, SidecarProvider, SIDECAR_CONNECT_TIMEOUT,
};
use super::stage::{Stage, StageContext, StageOutcome};
use super::state::{JobState, OrchestratorError};
use super::stream::{cancel_run, AbortOnDrop, SidecarTarget};
use super::synthesis::{PROGRESS_FLUSH_INTERVAL, PROGRESS_FLUSH_SEGMENTS};
use super::verify::{self, write_verify_report, Verdict, VerifyOutcome};
use crate::catalog::{Catalog, CatalogEntry, ModelTask};
use crate::hardware;
use crate::render::mp3::cache_wav_path;
use crate::sidecar::client::{post_sse, SseEvent};
use crate::storage::models::ModelInstall;
use crate::storage::{book, cache, job, models, settings};
use crate::words::{self, Timebase, WordsJson};

pub const FLAG_SKIPPED_NO_MODEL: &str = "align_skipped_no_model";
pub const FLAG_FAILED: &str = "align_failed";
pub const FLAG_EMPTY: &str = "align_empty";
/// The ONE align-default setting key: A22 preflight (synthesis) and the
/// align stage itself must resolve through this same name, or the advisory
/// can disagree with the stage that actually runs (Hermes S-3 MF13).
pub const DEFAULT_ALIGN_SETTING: &str = "default_model.align";
const TERMINAL_FLAGS: [&str; 3] = [FLAG_SKIPPED_NO_MODEL, FLAG_FAILED, FLAG_EMPTY];

fn has_terminal_align_flag(segment: &job::ScriptSegment) -> bool {
    let flags: Vec<String> = segment
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    flags.iter().any(|f| TERMINAL_FLAGS.contains(&f.as_str()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlignOverride {
    Skip,
    Model(String),
}

pub fn override_align_id(model_overrides_json: Option<&str>) -> Option<AlignOverride> {
    let raw = model_overrides_json?;
    let value: Value = serde_json::from_str(raw).ok()?;
    match value.get("align")? {
        Value::Null => Some(AlignOverride::Skip),
        Value::String(id) => Some(AlignOverride::Model(id.clone())),
        _ => None,
    }
}

fn assets_on_disk(entry: &CatalogEntry, install: &ModelInstall, models_root: &Path) -> bool {
    let install_dir = models_root.join(&install.path);
    entry
        .download
        .files
        .iter()
        .all(|f| install_dir.join(&f.dest_path).is_file())
}

fn is_wav2vec2_entry(entry: &CatalogEntry) -> bool {
    entry.family == "wav2vec2" || entry.id == "wav2vec2-base-960h"
}

pub fn is_english_language(lang: Option<&str>) -> bool {
    match lang {
        Some(s) => {
            let code = s
                .trim()
                .split(['-', '_'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            code.is_empty() || code == "en" || code == "english"
        }
        None => true,
    }
}

pub fn resolve_align_id(
    model_overrides_json: Option<&str>,
    default_align_setting: Option<&str>,
    catalog: &Catalog,
    installs: &[ModelInstall],
    models_root: &Path,
    book_language: Option<&str>,
) -> Option<String> {
    match override_align_id(model_overrides_json) {
        Some(AlignOverride::Skip) => return None,
        Some(AlignOverride::Model(id)) => return Some(id),
        None => {}
    }
    let is_en = is_english_language(book_language);
    if let Some(id) = default_align_setting {
        let is_w2v = catalog
            .models
            .iter()
            .find(|e| e.id == id)
            .is_some_and(is_wav2vec2_entry)
            || id.contains("wav2vec2");
        if is_en || !is_w2v {
            return Some(id.to_string());
        }
    }
    let mut candidates: Vec<&CatalogEntry> = catalog
        .models
        .iter()
        .filter(|entry| entry.task == ModelTask::Align && !entry.deprecated)
        .filter(|entry| is_en || !is_wav2vec2_entry(entry))
        .filter(|entry| {
            installs
                .iter()
                .find(|i| i.model_id == entry.id)
                .is_some_and(|install| assets_on_disk(entry, install, models_root))
        })
        .collect();
    candidates.sort_by_key(|entry| {
        let lang_preferred = if is_en && is_wav2vec2_entry(entry) {
            1
        } else {
            0
        };
        (
            std::cmp::Reverse(lang_preferred),
            std::cmp::Reverse(entry.speed_score),
        )
    });
    candidates.first().map(|entry| entry.id.clone())
}

pub const ALIGN_ALGO_VERSION: u32 = 2;

pub fn align_cache_key(audio_hash: &str, model_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(audio_hash.as_bytes());
    hasher.update(b":");
    hasher.update(model_id.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn align_words_cache_key_for_version(audio_hash: &str, model_id: &str, version: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(audio_hash.as_bytes());
    hasher.update(b":");
    hasher.update(model_id.as_bytes());
    hasher.update(b":v");
    hasher.update(version.to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn align_words_cache_key(audio_hash: &str, model_id: &str) -> String {
    align_words_cache_key_for_version(audio_hash, model_id, ALIGN_ALGO_VERSION)
}

pub struct AligningStage {
    client: reqwest::Client,
    sidecar: Arc<dyn SidecarProvider>,
    models_root: PathBuf,
    cache_dir: PathBuf,
    local_models_usable: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl AligningStage {
    pub fn with_local_models_check(
        mut self,
        usable: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.local_models_usable = Arc::new(usable);
        self
    }

    pub fn new(
        sidecar: Arc<dyn SidecarProvider>,
        models_root: PathBuf,
        cache_dir: PathBuf,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(SIDECAR_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();
        AligningStage {
            client,
            sidecar,
            models_root,
            cache_dir,
            local_models_usable: Arc::new(|| true),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawAlignResult {
    idx: i64,
    #[serde(default)]
    words: Vec<serde_json::Value>,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    words_timebase: Timebase,
}

impl AligningStage {
    async fn stage_plan(
        &self,
        ctx: &StageContext,
        record: &job::RenderJob,
        entry: &CatalogEntry,
    ) -> Result<serde_json::Value, OrchestratorError> {
        if let Some(stored) = super::scripting::stored_plan_for(
            record.align_plan_json.as_deref(),
            &entry.id,
            &ctx.job_id,
            "align",
        )? {
            if stored.upcast {
                let rewritten = job::rewrite_plan_if_unchanged(
                    &ctx.pool,
                    &ctx.job_id,
                    job::PlanColumn::Align,
                    record.align_plan_json.as_deref().unwrap_or_default(),
                    &stored.plan.to_string(),
                )
                .await?;
                if !rewritten {
                    tracing::info!(
                        "job {} kept a concurrent align plan write instead of its upcast",
                        ctx.job_id
                    );
                }
            }
            return Ok(stored.plan);
        }
        let profile = hardware::detect(
            &self.models_root,
            hardware::DirOrigin::Default,
            &self.models_root,
        )
        .await;
        let plan = serde_json::to_value(hardware::plan::align_fit(entry, &profile))?;
        job::set_job_align_plan(&ctx.pool, &ctx.job_id, &plan.to_string()).await?;
        residency_check::log_stage_plan(&ctx.job_id, JobState::Aligning, &plan).await;
        Ok(plan)
    }
}

impl Stage for AligningStage {
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
            let segments = job::list_segments(&ctx.pool, &ctx.job_id).await?;
            if segments.is_empty() {
                return Err(OrchestratorError::InvalidRequest(
                    "job has no segments to align".into(),
                ));
            }

            let verify = record.verify;
            let mut missing_audio_ids: Vec<&str> = Vec::new();
            for segment in &segments {
                if let Some(hash) = segment.audio_hash.as_deref() {
                    if !crate::render::mp3::is_valid_wav_artifact(&self.cache_dir, hash) {
                        missing_audio_ids.push(segment.id.as_str());
                    }
                }
            }
            if !missing_audio_ids.is_empty() {
                job::clear_segments_audio(&ctx.pool, &missing_audio_ids).await?;
                return Err(OrchestratorError::MissingAudioArtifact {
                    stage: JobState::Aligning,
                    count: missing_audio_ids.len(),
                    message: format!(
                        "{} audio artifact(s) missing from cache during alignment",
                        missing_audio_ids.len()
                    ),
                });
            }

            let mut verdicts: HashMap<String, Verdict> = HashMap::new();
            let total = segments.len() as i64;
            let mut pending: Vec<&job::ScriptSegment> = Vec::new();
            let mut preserve_words: HashSet<String> = HashSet::new();
            let mut scoring_only: HashSet<String> = HashSet::new();
            let mut no_audio = 0i64;
            let mut flagged = 0i64;
            let mut already_aligned = 0i64;
            for segment in &segments {
                if segment.audio_hash.is_none() {
                    no_audio += 1;
                    continue;
                }
                if has_terminal_align_flag(segment) {
                    flagged += 1;
                    continue;
                }
                if let Some(existing) = job::get_alignment(&ctx.pool, &segment.id).await? {
                    if !verify || existing.wer.is_some() {
                        already_aligned += 1;
                        continue;
                    }
                    scoring_only.insert(segment.id.clone());
                    if existing.source == "tts_native" {
                        preserve_words.insert(segment.id.clone());
                    }
                }
                pending.push(segment);
            }
            let done_before = total - pending.len() as i64;
            ctx.checkpoint.total_segments = total;
            ctx.checkpoint.done_segments = done_before;
            ctx.save_checkpoint().await?;
            if pending.is_empty() {
                tracing::info!(
                    "alignment skipped for job {}: {already_aligned} of {total} segment(s) \
                     already aligned, {no_audio} without audio, {flagged} terminally flagged",
                    ctx.job_id
                );
                let _ =
                    write_verify_report(&ctx.pool, &ctx.job_id, verify, &segments, &HashMap::new())
                        .await?;
                ctx.emit_progress(1.0, Some(0), "alignment cached");
                ctx.merge_report_duration("align", stage_started.elapsed().as_secs())
                    .await?;
                return Ok(StageOutcome::Completed);
            }

            let book = book::get_book(&ctx.pool, &record.book_id)
                .await?
                .ok_or_else(|| OrchestratorError::BookNotFound(record.book_id.clone()))?;
            let language = book.language.clone();

            let installs = models::list_model_installs(&ctx.pool).await?;
            let catalog = crate::catalog::bundled_catalog();
            let default_setting = settings::get_setting(&ctx.pool, DEFAULT_ALIGN_SETTING).await?;
            let Some(model_id) = resolve_align_id(
                record.model_overrides_json.as_deref(),
                default_setting.as_deref(),
                catalog,
                &installs,
                &self.models_root,
                language.as_deref(),
            ) else {
                tracing::info!(
                    job = %ctx.job_id,
                    "no align model resolved; skipping alignment"
                );
                let _ = write_verify_report(&ctx.pool, &ctx.job_id, verify, &segments, &verdicts)
                    .await?;
                return skip_all(ctx, &pending, total, &scoring_only).await;
            };
            if !(self.local_models_usable)() {
                tracing::info!("the running engine has no local models; skipping alignment");
                let _ = write_verify_report(&ctx.pool, &ctx.job_id, verify, &segments, &verdicts)
                    .await?;
                return skip_all(ctx, &pending, total, &scoring_only).await;
            }
            let Some(install) = installs.iter().find(|i| i.model_id == model_id) else {
                tracing::info!("align model '{model_id}' not installed; skipping alignment");
                let _ = write_verify_report(&ctx.pool, &ctx.job_id, verify, &segments, &verdicts)
                    .await?;
                return skip_all(ctx, &pending, total, &scoring_only).await;
            };
            let Some(entry) = catalog
                .models
                .iter()
                .find(|e| e.id == model_id && e.task == ModelTask::Align)
            else {
                tracing::info!("align model '{model_id}' not in catalog; skipping alignment");
                let _ = write_verify_report(&ctx.pool, &ctx.job_id, verify, &segments, &verdicts)
                    .await?;
                return skip_all(ctx, &pending, total, &scoring_only).await;
            };
            if !assets_on_disk(entry, install, &self.models_root) {
                tracing::info!("align model '{model_id}' files missing; skipping alignment");
                let _ = write_verify_report(&ctx.pool, &ctx.job_id, verify, &segments, &verdicts)
                    .await?;
                return skip_all(ctx, &pending, total, &scoring_only).await;
            }
            let model_dir = self.models_root.join(&install.path);

            let (port, token) = match await_connection(&*self.sidecar, &ctx.control).await? {
                ConnectionWait::Ready(port, token) => (port, token),
                ConnectionWait::Interrupted(outcome) => return Ok(outcome),
            };

            let resource_plan = declared_cpu_plan(self.stage_plan(ctx, &record, entry).await?);

            let body = json!({
                "segments": pending
                    .iter()
                    .map(|s| {
                        let hash = s.audio_hash.as_deref().unwrap_or_default();
                        json!({
                            "idx": s.idx,
                            "text": s.text,
                            "audio_path": cache_wav_path(&self.cache_dir, hash).to_string_lossy(),
                            "audio_hash": hash,
                        })
                    })
                    .collect::<Vec<_>>(),
                "model_id": model_id,
                "model_dir": model_dir.to_string_lossy(),
                "cache_dir": self.cache_dir.to_string_lossy(),
                "language": language,
                "resource_plan": resource_plan,
                "stage": JobState::Aligning.as_str(),
            });

            let pending_total = pending.len() as i64;
            let (tx, mut rx) = mpsc::unbounded_channel::<SseEvent>();
            let client = self.client.clone();
            let run_id = request_run_id(&ctx.job_id);
            let cancel_token = token.clone();
            let cancel_run_id = run_id.clone();
            let mut sse_task = AbortOnDrop(tokio::spawn(async move {
                post_sse(&client, port, &token, "/align", &run_id, &body, |event| {
                    let _ = tx.send(event);
                })
                .await
            }));

            let started = Instant::now();
            let mut control = ctx.control.clone();
            let mut processed: std::collections::HashSet<i64> = std::collections::HashSet::new();
            let mut done = done_before;
            let mut last_flushed = done_before;
            let mut last_flush_at = Instant::now();
            let mut last_label = "segment aligned";
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
                                serde_json::from_value::<RawAlignResult>(value)
                                    .map_err(|err| err.to_string())
                            });
                        let result = match raw {
                            Ok(result) => result,
                            Err(err) => {
                                parse_error = Some(err);
                                continue;
                            }
                        };
                        let Some(segment) = pending.iter().find(|s| s.idx == result.idx) else {
                            tracing::warn!(
                                "align result for unknown segment idx {}, skipping",
                                result.idx
                            );
                            continue;
                        };
                        if !processed.insert(result.idx) {
                            tracing::warn!(
                                "duplicate align result for segment idx {}, skipping",
                                result.idx
                            );
                            continue;
                        }
                        let succeeded = apply_result(
                            ctx,
                            segment,
                            &result,
                            &model_id,
                            &self.cache_dir,
                            preserve_words.contains(&segment.id),
                        )
                        .await?;
                        let read_back = !result.flags.iter().any(|flag| flag == FLAG_FAILED);
                        if verify && read_back {
                            let heard = result
                                .words
                                .iter()
                                .filter_map(|word| word["w"].as_str())
                                .collect::<Vec<_>>()
                                .join(" ");
                            let verdict = verify::score_segment(&ctx.pool, segment, &heard).await?;
                            verdicts.insert(segment.id.clone(), verdict);
                        }
                        done += 1;
                        last_label = if succeeded {
                            "segment aligned"
                        } else {
                            "segment alignment failed"
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
                            "align failed",
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
                let gpu_plan = crate::hardware::device::plan_device(&resource_plan)
                    .map(|device| device.is_gpu())
                    .unwrap_or(true);
                let propagate = match error.engine_code().map(classify_code) {
                    Some(StageFailure::Contention) => true,
                    Some(failure) if failure.recovers() => gpu_plan,
                    _ => false,
                };
                if propagate {
                    return Err(error);
                }
                let _ = write_verify_report(&ctx.pool, &ctx.job_id, verify, &segments, &verdicts)
                    .await?;
                tracing::warn!("align failed ({error}); sentence highlight fallback");
                let remaining: Vec<&job::ScriptSegment> = pending
                    .iter()
                    .filter(|s| !processed.contains(&s.idx))
                    .copied()
                    .collect();
                for segment in &remaining {
                    if scoring_only.contains(&segment.id) {
                        continue;
                    }
                    add_flags(ctx, segment, &[FLAG_FAILED.to_string()]).await?;
                }
                ctx.checkpoint.done_segments = total;
                ctx.save_checkpoint().await?;
                ctx.emit_progress(
                    1.0,
                    Some(0),
                    "alignment failed; sentence highlight fallback",
                );
                ctx.merge_report_duration("align", stage_started.elapsed().as_secs())
                    .await?;
                return Ok(StageOutcome::Completed);
            }
            if let Some(err) = parse_error {
                return Err(OrchestratorError::Sidecar(format!(
                    "failed to parse align result: {err}"
                )));
            }
            ctx.merge_report_flags("align_flags", &stage_flags).await?;
            match (&mut sse_task.0).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => return Err(OrchestratorError::from(err)),
                Err(err) => {
                    return Err(OrchestratorError::Sidecar(format!(
                        "align stream task failed: {err}"
                    )))
                }
            }
            if !finished {
                return Err(OrchestratorError::Sidecar(
                    "align stream ended without completion".into(),
                ));
            }

            let missing: Vec<&job::ScriptSegment> = pending
                .iter()
                .filter(|s| !processed.contains(&s.idx))
                .copied()
                .collect();
            for segment in &missing {
                if scoring_only.contains(&segment.id) {
                    continue;
                }
                tracing::warn!(
                    "no align result for segment idx {}; sentence highlight fallback",
                    segment.idx
                );
                add_flags(ctx, segment, &[FLAG_FAILED.to_string()]).await?;
            }
            ctx.checkpoint.done_segments = total;
            ctx.save_checkpoint().await?;

            let outcome =
                write_verify_report(&ctx.pool, &ctx.job_id, verify, &segments, &verdicts).await?;
            let label = match outcome {
                VerifyOutcome::NothingScored { scoreable } => {
                    format!(
                        "{done}/{total} segments aligned; accuracy check scored 0 of {scoreable}"
                    )
                }
                _ => format!("{done}/{total} segments aligned"),
            };
            tracing::info!(job = %ctx.job_id, "align summary: {label}");
            ctx.emit_progress(1.0, Some(0), &label);
            ctx.merge_report_duration("align", stage_started.elapsed().as_secs())
                .await?;
            Ok(StageOutcome::Completed)
        })
    }
}

fn declared_cpu_plan(plan: serde_json::Value) -> serde_json::Value {
    match hardware::device::plan_device(&plan) {
        Ok(device) if device.is_gpu() => {
            tracing::info!(
                "align runs the int8 build on the cpu; clamping a stored gpu align plan"
            );
            hardware::degrade::cpu_fallback(&plan)
        }
        _ => plan,
    }
}

async fn skip_all(
    ctx: &mut StageContext,
    pending: &[&job::ScriptSegment],
    total: i64,
    scoring_only: &HashSet<String>,
) -> Result<StageOutcome, OrchestratorError> {
    for segment in pending {
        if scoring_only.contains(&segment.id) {
            continue;
        }
        add_flags(ctx, segment, &[FLAG_SKIPPED_NO_MODEL.to_string()]).await?;
    }
    ctx.checkpoint.done_segments = total;
    ctx.save_checkpoint().await?;
    ctx.emit_progress(1.0, Some(0), "no align model; sentence highlight fallback");
    Ok(StageOutcome::Completed)
}

async fn add_flags(
    ctx: &StageContext,
    segment: &job::ScriptSegment,
    new_flags: &[String],
) -> Result<(), OrchestratorError> {
    let mut flags: Vec<String> = segment
        .flags_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    let mut changed = false;
    for flag in new_flags {
        if !flags.contains(flag) {
            flags.push(flag.clone());
            changed = true;
        }
    }
    if changed {
        job::set_segment_flags(&ctx.pool, &segment.id, &serde_json::to_string(&flags)?).await?;
    }
    Ok(())
}

async fn apply_result(
    ctx: &StageContext,
    segment: &job::ScriptSegment,
    result: &RawAlignResult,
    model_id: &str,
    cache_dir: &Path,
    preserve_words: bool,
) -> Result<bool, OrchestratorError> {
    add_flags(ctx, segment, &result.flags).await?;
    if result.words.is_empty() {
        return Ok(false);
    }
    let words_json = WordsJson::declare(&result.words, result.words_timebase)?;
    if !preserve_words {
        let alignment = job::Alignment {
            segment_id: segment.id.clone(),
            source: words::SOURCE_WHISPERX.to_string(),
            words_json: words_json.clone(),
            wer: None,
        };
        job::upsert_alignment(&ctx.pool, &alignment).await?;
    }
    if let Some(audio_hash) = segment.audio_hash.as_deref() {
        claim_wav(ctx, audio_hash, cache_dir).await?;
        let key = align_words_cache_key(audio_hash, model_id);
        let bytes = std::fs::metadata(cache_wav_path(cache_dir, &key))
            .map(|m| m.len() as i64)
            .unwrap_or(words_json.as_str().len() as i64);
        cache::record_use(&ctx.pool, &key, "align", bytes, Some(&ctx.job_id)).await?;
    }
    Ok(true)
}

pub async fn claim_wav(
    ctx: &StageContext,
    audio_hash: &str,
    cache_dir: &Path,
) -> Result<(), OrchestratorError> {
    let Ok(meta) = std::fs::metadata(cache_wav_path(cache_dir, audio_hash)) else {
        return Ok(());
    };
    cache::record_use(
        &ctx.pool,
        audio_hash,
        "wav",
        meta.len() as i64,
        Some(&ctx.job_id),
    )
    .await?;
    Ok(())
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
    let align_done = done - done_before;
    let eta_seconds = if align_done > 0 && pending_total > align_done {
        let rate = started.elapsed().as_secs_f64() / align_done as f64;
        Some((rate * (pending_total - align_done) as f64) as i64)
    } else {
        None
    };
    ctx.emit_progress(done as f64 / total as f64, eta_seconds, label);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::bundled_catalog;

    const FROZEN_NATIVE_KEY: &str =
        "deea1f9deb5c0befa0a4cfc666b1cf82f1b3d9876694d1393c73be20490181aa";
    const FROZEN_WHISPERX_KEY: &str =
        "3fbb49d419fc314be6aa4dbe09971fa879d6376bc6ac162705f11cf41ffd6868";

    #[test]
    fn the_align_words_cache_key_changes_with_the_version() {
        let key_v1 = align_words_cache_key_for_version(&"a".repeat(64), "whisperx-small-int8", 1);
        let key_v2 = align_words_cache_key_for_version(&"a".repeat(64), "whisperx-small-int8", 2);
        assert_ne!(key_v1, key_v2);
        assert_eq!(
            align_words_cache_key(&"a".repeat(64), "whisperx-small-int8"),
            key_v2
        );
    }

    #[test]
    fn the_align_cache_key_is_frozen_and_agrees_with_the_engine() {
        const REASON: &str = concat!(
            "the words envelope is versioned under UNCHANGED cache keys on purpose: bumping ",
            "the key makes every cached word list unreachable, and on a WAV cache hit the tts ",
            "path returns before the backend runs, so the load misses, the words come back ",
            "empty, the writer skips an empty list, and the stale row survives while the fresh ",
            "words are lost (TASKS.md R-T). Both languages derive this key, so these literals ",
            "are also the cross-language guard. Change them only with a migration for the ",
            "orphaned cache_entry rows."
        );
        assert_eq!(
            align_cache_key(&"a".repeat(64), "tts_native"),
            FROZEN_NATIVE_KEY,
            "{REASON}"
        );
        assert_eq!(
            align_words_cache_key(&"a".repeat(64), "whisperx-small-int8"),
            FROZEN_WHISPERX_KEY
        );
    }

    fn install(model_id: &str) -> ModelInstall {
        ModelInstall {
            model_id: model_id.to_string(),
            path: format!("align/{model_id}/"),
            sha256: "deadbeef".into(),
            disk_bytes: 1,
            source: "curated".into(),
            installed_at: 0,
        }
    }

    fn materialize_assets(models_root: &Path, model_id: &str) {
        let entry = bundled_catalog()
            .models
            .iter()
            .find(|e| e.id == model_id)
            .expect("entry");
        let dir = models_root.join(format!("align/{model_id}/"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for file in &entry.download.files {
            std::fs::write(dir.join(&file.dest_path), b"stub").expect("write");
        }
    }

    #[test]
    fn override_wins() {
        let dir = tempfile::tempdir().unwrap();
        let id = resolve_align_id(
            Some(r#"{"align": "whisperx-large-v3"}"#),
            None,
            bundled_catalog(),
            &[],
            dir.path(),
            None,
        );
        assert_eq!(id.as_deref(), Some("whisperx-large-v3"));
    }

    #[test]
    fn null_override_skips() {
        let dir = tempfile::tempdir().unwrap();
        materialize_assets(dir.path(), "whisperx-small-int8");
        let id = resolve_align_id(
            Some(r#"{"align": null}"#),
            None,
            bundled_catalog(),
            &[install("whisperx-small-int8")],
            dir.path(),
            None,
        );
        assert_eq!(id, None);
    }

    #[test]
    fn setting_beats_installed() {
        let dir = tempfile::tempdir().unwrap();
        materialize_assets(dir.path(), "whisperx-small-int8");
        let id = resolve_align_id(
            None,
            Some("whisperx-large-v3"),
            bundled_catalog(),
            &[install("whisperx-small-int8")],
            dir.path(),
            None,
        );
        assert_eq!(id.as_deref(), Some("whisperx-large-v3"));
    }

    #[test]
    fn best_installed_fallback_prefers_speed() {
        let dir = tempfile::tempdir().unwrap();
        materialize_assets(dir.path(), "whisperx-small-int8");
        materialize_assets(dir.path(), "whisperx-large-v3");
        let id = resolve_align_id(
            None,
            None,
            bundled_catalog(),
            &[install("whisperx-small-int8"), install("whisperx-large-v3")],
            dir.path(),
            None,
        );
        assert_eq!(id.as_deref(), Some("whisperx-small-int8"));
    }

    #[test]
    fn missing_assets_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let id = resolve_align_id(
            None,
            None,
            bundled_catalog(),
            &[install("whisperx-small-int8")],
            dir.path(),
            None,
        );
        assert_eq!(id, None);
    }

    #[test]
    fn language_aware_default_english_prefers_wav2vec2() {
        let dir = tempfile::tempdir().unwrap();
        materialize_assets(dir.path(), "whisperx-small-int8");
        materialize_assets(dir.path(), "wav2vec2-base-960h");
        let installs = [
            install("whisperx-small-int8"),
            install("wav2vec2-base-960h"),
        ];

        for lang in [
            Some("en"),
            Some("en-US"),
            Some("en-GB"),
            Some("english"),
            None,
        ] {
            let id = resolve_align_id(None, None, bundled_catalog(), &installs, dir.path(), lang);
            assert_eq!(
                id.as_deref(),
                Some("wav2vec2-base-960h"),
                "failed for language {lang:?}"
            );
        }
    }

    #[test]
    fn language_aware_default_non_english_keeps_whisperx() {
        let dir = tempfile::tempdir().unwrap();
        materialize_assets(dir.path(), "whisperx-small-int8");
        materialize_assets(dir.path(), "wav2vec2-base-960h");
        let installs = [
            install("whisperx-small-int8"),
            install("wav2vec2-base-960h"),
        ];

        for lang in [Some("ar"), Some("fr"), Some("de"), Some("es"), Some("ja")] {
            let id = resolve_align_id(None, None, bundled_catalog(), &installs, dir.path(), lang);
            assert_eq!(
                id.as_deref(),
                Some("whisperx-small-int8"),
                "failed for language {lang:?}"
            );
        }
    }

    #[test]
    fn language_aware_default_flexibility_first_override_selectable() {
        let dir = tempfile::tempdir().unwrap();
        materialize_assets(dir.path(), "whisperx-small-int8");
        materialize_assets(dir.path(), "wav2vec2-base-960h");
        let installs = [
            install("whisperx-small-int8"),
            install("wav2vec2-base-960h"),
        ];

        let id = resolve_align_id(
            Some(r#"{"align": "whisperx-small-int8"}"#),
            None,
            bundled_catalog(),
            &installs,
            dir.path(),
            Some("en"),
        );
        assert_eq!(id.as_deref(), Some("whisperx-small-int8"));
    }

    #[test]
    fn language_aware_default_english_falls_back_when_wav2vec2_absent() {
        let dir = tempfile::tempdir().unwrap();
        materialize_assets(dir.path(), "whisperx-small-int8");
        let installs = [install("whisperx-small-int8")];

        let id = resolve_align_id(
            None,
            None,
            bundled_catalog(),
            &installs,
            dir.path(),
            Some("en"),
        );
        assert_eq!(id.as_deref(), Some("whisperx-small-int8"));
    }

    #[test]
    fn language_aware_default_setting_wav2vec2_ignored_for_non_english() {
        let dir = tempfile::tempdir().unwrap();
        materialize_assets(dir.path(), "whisperx-small-int8");
        materialize_assets(dir.path(), "wav2vec2-base-960h");
        let installs = [
            install("whisperx-small-int8"),
            install("wav2vec2-base-960h"),
        ];

        let id = resolve_align_id(
            None,
            Some("wav2vec2-base-960h"),
            bundled_catalog(),
            &installs,
            dir.path(),
            Some("ar"),
        );
        assert_eq!(id.as_deref(), Some("whisperx-small-int8"));
    }

    #[test]
    fn cache_key_matches_engine_format() {
        let native_key = align_cache_key("abc", "tts_native");
        let mut native_hasher = Sha256::new();
        native_hasher.update(b"abc:tts_native");
        assert_eq!(native_key, format!("{:x}", native_hasher.finalize()));

        let key = align_words_cache_key("abc", "whisperx-small-int8");
        let mut hasher = Sha256::new();
        hasher.update(format!("abc:whisperx-small-int8:v{ALIGN_ALGO_VERSION}").as_bytes());
        assert_eq!(key, format!("{:x}", hasher.finalize()));
    }
}
