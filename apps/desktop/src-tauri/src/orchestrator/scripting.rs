use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::contention::{
    classify_failure, record_contention, ContentionDelta, StageFailure, UNLOAD_DRAIN_DELAY,
    UNLOAD_DRAIN_RETRIES,
};
use super::layout::scoped_pages;
use super::llm::{fallback_cpu_plan, resolve_llm_id};
use super::residency_check;
use super::stage::{Stage, StageContext, StageOutcome};
use super::state::{JobState, OrchestratorError, Scope};
use super::stream::{post_sse_cancellable, SidecarTarget, StreamOutcome};
use crate::catalog::{self, bundled_catalog};
use crate::hardware;
use crate::sidecar::client::{post_json, SseEvent};
use crate::storage::{book, job, models, new_id, settings};

pub trait SidecarProvider: Send + Sync {
    fn connection(&self) -> Option<(u16, String)>;
    fn respawn(&self) {}
}

pub const SIDECAR_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const SIDECAR_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
pub const SIDECAR_READY_POLL: std::time::Duration = std::time::Duration::from_millis(250);

pub fn request_run_id(job_id: &str) -> String {
    format!("{job_id}:{}", new_id())
}

pub enum ConnectionWait {
    Ready(u16, String),
    Interrupted(StageOutcome),
}

pub async fn await_connection(
    provider: &dyn SidecarProvider,
    control: &super::stage::ControlReceiver,
) -> Result<ConnectionWait, OrchestratorError> {
    let deadline = tokio::time::Instant::now() + SIDECAR_READY_TIMEOUT;
    loop {
        if let Some((port, token)) = provider.connection() {
            return Ok(ConnectionWait::Ready(port, token));
        }
        if let Some(outcome) = control.interruption() {
            return Ok(ConnectionWait::Interrupted(outcome));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(OrchestratorError::SidecarUnavailable);
        }
        tokio::time::sleep(SIDECAR_READY_POLL).await;
    }
}

pub struct ScriptingStage {
    client: reqwest::Client,
    sidecar: Arc<dyn SidecarProvider>,
    models_root: PathBuf,
    drain_delay: std::time::Duration,
    /// VA3 key custody for cloud script bindings (S-1d). `None` (tests,
    /// minimal harnesses) still serves loopback keyless connections; a keyed
    /// binding then refuses with the reason instead of running keyless.
    keys: Option<Arc<crate::voice::keys::KeyManager>>,
    patience_config: Option<super::overload::PatienceConfig>,
}

impl ScriptingStage {
    pub fn new(sidecar: Arc<dyn SidecarProvider>, models_root: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(SIDECAR_CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default();
        ScriptingStage {
            client,
            sidecar,
            models_root,
            drain_delay: UNLOAD_DRAIN_DELAY,
            keys: None,
            patience_config: None,
        }
    }

    pub fn with_drain_delay(mut self, drain_delay: std::time::Duration) -> Self {
        self.drain_delay = drain_delay;
        self
    }

    pub fn with_key_manager(mut self, keys: Arc<crate::voice::keys::KeyManager>) -> Self {
        self.keys = Some(keys);
        self
    }

    pub fn with_patience_config(mut self, config: super::overload::PatienceConfig) -> Self {
        self.patience_config = Some(config);
        self
    }

    async fn prepare_llm(
        &self,
        ctx: &mut StageContext,
        record: &job::RenderJob,
        binding_model: Option<&str>,
        required: bool,
    ) -> Result<Option<(String, serde_json::Value, PathBuf)>, OrchestratorError> {
        match self.prepare_llm_inner(ctx, record, binding_model).await {
            Ok(prepared) => Ok(Some(prepared)),
            Err(OrchestratorError::ModelNotInstalled(id)) if !required => {
                tracing::info!("no usable llm ({id}); narrator falls back to rule normalization");
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    async fn prepare_llm_inner(
        &self,
        ctx: &mut StageContext,
        record: &job::RenderJob,
        binding_model: Option<&str>,
    ) -> Result<(String, serde_json::Value, PathBuf), OrchestratorError> {
        let installs = models::list_model_installs(&ctx.pool).await?;
        let catalog = bundled_catalog();
        // A recorded structural Local binding names the model verbatim
        // (charter §4.4); the legacy "llm" override and quality-ranked
        // default only apply when no binding was recorded.
        let model_id = match binding_model {
            Some(id) => id.to_string(),
            None => resolve_llm_id(record.model_overrides_json.as_deref(), catalog, &installs)?,
        };
        let install = installs
            .iter()
            .find(|i| i.model_id == model_id)
            .ok_or_else(|| OrchestratorError::ModelNotInstalled(model_id.clone()))?;
        let entry = catalog
            .models
            .iter()
            .find(|e| e.id == model_id)
            .ok_or_else(|| OrchestratorError::ModelNotInstalled(model_id.clone()))?;
        let gguf_file = super::vision_model::text_gguf_file(entry)
            .ok_or_else(|| OrchestratorError::ModelNotInstalled(model_id.clone()))?;
        let model_path = self
            .models_root
            .join(&install.path)
            .join(&gguf_file.dest_path);

        let stored = stored_plan_for(
            record.resource_plan_json.as_deref(),
            &model_id,
            &ctx.job_id,
            "scripting",
        )?;
        let plan = match stored {
            Some(stored) => {
                if stored.upcast {
                    let rewritten = job::rewrite_plan_if_unchanged(
                        &ctx.pool,
                        &ctx.job_id,
                        job::PlanColumn::Resource,
                        record.resource_plan_json.as_deref().unwrap_or_default(),
                        &stored.plan.to_string(),
                    )
                    .await?;
                    if !rewritten {
                        tracing::info!(
                            "job {} kept a concurrent scripting plan write instead of its upcast",
                            ctx.job_id
                        );
                    }
                }
                stored.plan
            }
            None => {
                let plan = compute_llm_plan(entry, &model_path, &self.models_root, &model_id).await;
                job::set_job_resource_plan(&ctx.pool, &ctx.job_id, &plan.to_string()).await?;
                residency_check::log_stage_plan(&ctx.job_id, JobState::Scripting, &plan).await;
                plan
            }
        };
        Ok((model_id, plan, model_path))
    }
}

pub(super) fn stored_plan_for(
    raw: Option<&str>,
    model_id: &str,
    job_id: &str,
    stage: &str,
) -> Result<Option<crate::hardware::device::StoredPlan>, OrchestratorError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let stored = match crate::hardware::device::read_stored_plan(raw) {
        Ok(stored) => stored,
        Err(crate::hardware::device::PlanReadError::TooNew(revision)) => {
            tracing::warn!(
                job = %job_id,
                "stage {stage} refusing a plan at revision {revision}, newer than this build"
            );
            return Err(OrchestratorError::InvalidRequest(format!(
                "ERR_PLAN: job {job_id} has a {stage} plan at revision {revision}, newer than                  this build understands; refusing to overwrite it"
            )));
        }
        Err(error) => {
            tracing::warn!("job {job_id} re-planning {stage}: {error}");
            return Ok(None);
        }
    };
    Ok((stored.plan.get("model_id").and_then(|v| v.as_str()) == Some(model_id)).then_some(stored))
}

pub(super) async fn compute_llm_plan(
    entry: &catalog::CatalogEntry,
    model_path: &std::path::Path,
    models_root: &std::path::Path,
    model_id: &str,
) -> serde_json::Value {
    let gguf_info = catalog::gguf::read_model_info(model_path).ok();
    let profile = hardware::detect(models_root, hardware::DirOrigin::Default, models_root).await;
    plan_from_profile(entry, &profile, gguf_info.as_ref(), model_id)
}

pub(super) fn plan_from_profile(
    entry: &catalog::CatalogEntry,
    profile: &hardware::HardwareProfile,
    gguf_info: Option<&catalog::gguf::GgufModelInfo>,
    model_id: &str,
) -> serde_json::Value {
    plan_json_from_fit(
        hardware::plan::compute_fit(entry, profile, gguf_info),
        model_id,
        gguf_info,
        &profile.ram,
    )
}

pub(super) fn plan_json_from_fit(
    fit: hardware::plan::ModelFit,
    model_id: &str,
    gguf_info: Option<&catalog::gguf::GgufModelInfo>,
    ram: &hardware::RamInfo,
) -> serde_json::Value {
    let gate_mb = hardware::plan::ram_gate_mb(ram);
    match fit.plan {
        // Serialising a `ResourcePlan` cannot realistically fail, but if it does
        // the fallback is what ships — and it used to ship badged Orange with
        // nothing checked. Both arms are charged now; there is no ungated arm.
        Some(plan) => serde_json::to_value(&plan)
            .unwrap_or_else(|_| fallback_cpu_plan(model_id, gguf_info, gate_mb)),
        // The planner refused every ctx rung at batch 512. The fallback is a
        // strictly smaller plan than any it tried, so its badge is decided by
        // charging it rather than by which arm reached it: stamping red over a
        // plan whose numbers say it fits is the same class of error as emitting
        // one whose numbers say it does not, and it makes red mean nothing.
        None => fallback_cpu_plan(model_id, gguf_info, gate_mb),
    }
}

#[derive(Debug, Deserialize)]
struct ModelLoadResponse {
    #[allow(dead_code)]
    model_id: String,
    #[allow(dead_code)]
    loaded: bool,
}

#[derive(Debug, Deserialize)]
struct ModelUnloadResponse {
    #[allow(dead_code)]
    model_id: String,
    #[allow(dead_code)]
    unloaded: bool,
}

const DECORATIVE_REGION_TYPE: &str = "decorative";
const NARRATOR_LLM_NORMALIZE_SETTING: &str = "narrator.llm_normalize";
const OFFLOAD_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const CAPABILITY_PENDING_POLL: std::time::Duration = std::time::Duration::from_millis(500);
const CAPABILITY_PENDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

enum CapabilityState {
    Known(bool),
    Pending,
    Unknown,
}

async fn fetch_capability(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    capability: &str,
) -> CapabilityState {
    let url = format!("http://127.0.0.1:{port}/health");
    let request = client
        .get(&url)
        .bearer_auth(token)
        .timeout(OFFLOAD_PROBE_TIMEOUT)
        .send();
    let Ok(response) = request.await else {
        return CapabilityState::Unknown;
    };
    let Ok(body) = response.json::<serde_json::Value>().await else {
        return CapabilityState::Unknown;
    };
    match body.get(capability) {
        Some(serde_json::Value::Bool(value)) => CapabilityState::Known(*value),
        Some(serde_json::Value::Null) => CapabilityState::Pending,
        _ => CapabilityState::Unknown,
    }
}

pub async fn engine_capability(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    capability: &str,
    default: bool,
) -> bool {
    engine_capability_known(client, port, token, capability)
        .await
        .unwrap_or(default)
}

pub async fn engine_capability_known(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    capability: &str,
) -> Option<bool> {
    let deadline = tokio::time::Instant::now() + CAPABILITY_PENDING_TIMEOUT;
    loop {
        match fetch_capability(client, port, token, capability).await {
            CapabilityState::Known(value) => return Some(value),
            CapabilityState::Unknown => return None,
            CapabilityState::Pending => {
                if tokio::time::Instant::now() >= deadline {
                    return None;
                }
                tokio::time::sleep(CAPABILITY_PENDING_POLL).await;
            }
        }
    }
}

pub(super) fn is_gpu_device(plan: &serde_json::Value) -> bool {
    match crate::hardware::device::plan_device(plan) {
        Ok(device) => device.is_gpu(),
        Err(reason) => {
            tracing::warn!("{reason}; treating the plan as GPU so the clamp still runs");
            true
        }
    }
}

pub(super) async fn clamp_plan_to_capability(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    plan: serde_json::Value,
    capability: &str,
    fail_open: bool,
) -> serde_json::Value {
    if !is_gpu_device(&plan) {
        return plan;
    }
    if engine_capability(client, port, token, capability, fail_open).await {
        return plan;
    }
    tracing::info!("engine runtime lacks {capability}; clamping plan to cpu");
    crate::hardware::degrade::cpu_fallback(&plan)
}

pub(super) async fn clamp_plan_to_runtime(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    plan: serde_json::Value,
) -> serde_json::Value {
    clamp_plan_to_capability(client, port, token, plan, "gpu_offload", true).await
}

async fn load_llm(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    model_path: &std::path::Path,
    plan: &serde_json::Value,
) -> Result<(), OrchestratorError> {
    let body = json!({
        "model_path": model_path.to_string_lossy(),
        "resource_plan": plan,
    });
    let _: ModelLoadResponse = post_json(client, port, token, "/models/load", &body)
        .await
        .map_err(OrchestratorError::from)?;
    Ok(())
}

async fn unload_llm(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    model_id: &str,
) -> Result<(), OrchestratorError> {
    let body = json!({ "model_id": model_id });
    post_json::<_, ModelUnloadResponse>(client, port, token, "/models/unload", &body)
        .await
        .map(|_| ())
        .map_err(OrchestratorError::from)
}

async fn unload_llm_draining(
    ctx: &StageContext,
    client: &reqwest::Client,
    port: u16,
    token: &str,
    model_id: &str,
    drain_delay: std::time::Duration,
) -> Result<(), OrchestratorError> {
    let control = &ctx.control;
    let started = std::time::Instant::now();
    let mut retries = 0;
    let mut contended_once = false;
    let drained = loop {
        let error = match unload_llm(client, port, token, model_id).await {
            Ok(()) => break Ok(()),
            Err(error) => error,
        };
        let contended = classify_failure(&error) == StageFailure::Contention;
        if !contended || control.interruption().is_some() {
            break Err(error);
        }
        contended_once = true;
        if retries == UNLOAD_DRAIN_RETRIES {
            break Err(OrchestratorError::EngineBusy {
                stage: JobState::Scripting,
                retries,
                waited_s: started.elapsed().as_secs(),
                cause: error.to_string(),
            });
        }
        tracing::info!(
            "stage SCRIPTING waiting {}ms for the llm slot to drain (retry {}): {error}",
            drain_delay.as_millis(),
            retries + 1
        );
        let mut watcher = control.clone();
        tokio::select! {
            _ = tokio::time::sleep(drain_delay) => {}
            _ = watcher.interrupted() => {}
        }
        retries += 1;
    };
    if contended_once {
        let recorded = record_contention(
            &ctx.pool,
            &ctx.job_id,
            JobState::Scripting,
            ContentionDelta {
                episodes: 1,
                retries: u64::from(retries),
                waited_s: started.elapsed().as_secs(),
            },
        )
        .await;
        if let Err(error) = recorded {
            tracing::warn!("contention telemetry failed for the scripting unload drain: {error}");
        }
    }
    drained
}

#[derive(Debug, Deserialize)]
struct RawSegment {
    #[serde(default)]
    chapter_idx: i64,
    page_no: i64,
    kind: String,
    text: String,
    #[serde(default)]
    source_span: serde_json::Value,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    figure_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawSuppressed {
    page_no: i64,
    role: String,
    reason: String,
    certainty: String,
    text: String,
    words: i64,
}

pub async fn narrator_llm_enabled(pool: &sqlx::SqlitePool) -> Result<bool, OrchestratorError> {
    let raw = settings::get_setting(pool, NARRATOR_LLM_NORMALIZE_SETTING).await?;
    Ok(matches!(
        raw.as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("true") | Some("1") | Some("yes")
    ))
}

/// Are this book's stored blocks present and whole?
///
/// Deliberately not "current": presence and currency were the same predicate under one
/// name, which is how a stale generation became invisible. Currency is
/// `library::generation_is_current`, it is reported on the blocks line, and it never
/// triggers a re-extraction on its own — a 923-page pass is 3.5 minutes on a GPU and 2.9
/// hours on CPU, so it is the user's decision, taken through `improve_extraction`.
fn blocks_are_present(book: &book::Book) -> bool {
    let Some(page_count) = book.page_count.filter(|count| *count > 0) else {
        return false;
    };
    book.blocks_version.is_some() && book.blocks_page_count == Some(page_count)
}

#[allow(clippy::too_many_arguments)]
async fn ensure_canonical_blocks(
    pool: &crate::storage::SqlitePool,
    client: &reqwest::Client,
    port: u16,
    token: &str,
    book: book::Book,
    models_root: &std::path::Path,
    fallback: &crate::ingest_fallback::FallbackSource,
    ctx: &StageContext,
) -> Result<Result<book::Book, StageOutcome>, OrchestratorError> {
    if blocks_are_present(&book) {
        return Ok(Ok(book));
    }
    let total = book.page_count.unwrap_or_default().max(1);
    let reason = match (&book.blocks_version, book.blocks_page_count) {
        (None, _) => "no stored generation".to_string(),
        (Some(version), Some(stored)) => {
            format!("generation covers {stored} of {total} page(s) (version={version})")
        }
        (Some(version), None) => format!("generation has no page count (version={version})"),
    };
    tracing::info!(
        "stage SCRIPTING backfill: {reason}; re-extracting '{}'",
        book.title
    );
    ctx.emit_progress(0.0, None, "re-extracting text");
    let mut control = ctx.control.clone();
    let backfill = crate::library::backfill_blocks(
        pool,
        crate::orchestrator::stream::SidecarTarget::new(client, port, token),
        &book,
        crate::library::docling_artifacts_dir(models_root),
        crate::library::BackfillIntent::for_job(book.layout_analysed),
        fallback,
        &mut control,
        |page, pages| {
            let pct = if pages > 0 {
                (page as f64 / pages as f64).clamp(0.0, 1.0) * 0.2
            } else {
                0.0
            };
            ctx.emit_progress(
                pct,
                None,
                &format!("re-extracting text (page {page}/{total})"),
            );
        },
    )
    .await
    .map_err(OrchestratorError::Sidecar)?;
    if let crate::library::BackfillResult::Interrupted(outcome) = backfill {
        return Ok(Err(outcome));
    }
    book::get_book(pool, &book.id)
        .await?
        .ok_or_else(|| OrchestratorError::BookNotFound(book.id.clone()))
        .map(Ok)
}

/// The one line that says which text this run is reading, and how old it is.
///
/// `generation=stale` is the whole point: a re-create on a book extracted by an older
/// build reads the blocks that build stored, so a listener complaint about cues the fix
/// was supposed to close is answerable from the log without re-running anything. Nothing
/// acts on it — the re-read is offered in the library, never taken inside a job.
fn format_blocks_line(
    pages: usize,
    flagged: usize,
    version: Option<&str>,
    generation_current: bool,
) -> String {
    let generation = if generation_current {
        "current"
    } else {
        "stale"
    };
    format!(
        "stage SCRIPTING blocks: pages={pages} flagged={flagged} version={} \
generation={generation}",
        version.unwrap_or("none")
    )
}

const BLOCKS_MISSING_CODE: &str = "ERR_BLOCKS_MISSING";

async fn canonical_pages(
    pool: &crate::storage::SqlitePool,
    book: &book::Book,
    expected_generation: Option<&str>,
) -> Result<Vec<serde_json::Value>, OrchestratorError> {
    if !blocks_are_present(book) {
        return Err(OrchestratorError::Engine {
            status: None,
            code: BLOCKS_MISSING_CODE.to_string(),
            message: "this book was imported before canonical text extraction; re-import it \
                      to generate audio"
                .to_string(),
        });
    }
    let rows = book::list_pages(pool, &book.id).await?;
    let flagged = rows.iter().filter(|page| page.needs_upgrade).count();
    tracing::debug!(
        "{}",
        format_blocks_line(
            rows.len(),
            flagged,
            book.blocks_version.as_deref(),
            crate::library::generation_is_current(
                book.blocks_version.as_deref(),
                expected_generation
            ),
        )
    );
    let pages: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|page| {
            json!({
                "page_no": page.page_no,
                "width_pt": page.width_pt.unwrap_or_default(),
                "height_pt": page.height_pt.unwrap_or_default(),
                "text_json": page.text_json,
            })
        })
        .collect();
    if pages.is_empty() {
        return Err(OrchestratorError::Engine {
            status: None,
            code: BLOCKS_MISSING_CODE.to_string(),
            message: "this book has no stored text blocks".to_string(),
        });
    }
    Ok(pages)
}

fn figure_caption_body(figure: &book::FigureRegion) -> Option<serde_json::Value> {
    if figure.kind == DECORATIVE_REGION_TYPE {
        return None;
    }
    let bbox: [f64; 4] = serde_json::from_str(&figure.bbox_json).ok()?;
    let [x0, y0, x1, y1] = bbox;
    if !bbox.iter().all(|value| value.is_finite()) || x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(json!({
        "region_id": figure.id,
        "page_no": figure.page_no,
        "bbox": bbox,
        "region_type": figure.kind,
        "caption": figure.caption,
        "explanation": figure.explanation,
        "confidence": figure.confidence,
    }))
}

#[derive(Debug, Serialize)]
struct ChapterBound {
    idx: i64,
    page_start: i64,
    page_end: i64,
}

impl Stage for ScriptingStage {
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
            let book = book::get_book(&ctx.pool, &record.book_id)
                .await?
                .ok_or_else(|| OrchestratorError::BookNotFound(record.book_id.clone()))?;
            let chapters = book::list_chapters(&ctx.pool, &record.book_id).await?;

            let existing = job::list_segments(&ctx.pool, &ctx.job_id).await?;
            if !existing.is_empty() {
                ctx.checkpoint.paused_reason = None;
                ctx.checkpoint.total_segments = existing.len() as i64;
                ctx.checkpoint.done_segments = existing.len() as i64;
                ctx.save_checkpoint().await?;
                if let Err(error) =
                    super::synthesis_cloud::strip_report_paused_reason(&ctx.pool, &ctx.job_id).await
                {
                    tracing::warn!(
                        job = %ctx.job_id,
                        "failed to clear the persisted pause reason: {error}"
                    );
                }
                ctx.emit_progress(1.0, Some(0), "script cached");
                ctx.merge_report_duration("script", stage_started.elapsed().as_secs())
                    .await?;
                return Ok(StageOutcome::Completed);
            }

            let scope: Scope = serde_json::from_str(&record.scope_json)?;
            let teacher = record.mode == "teacher";
            let binding = super::scripting_cloud::override_script_binding(
                record.model_overrides_json.as_deref(),
            )?;
            let cloud_binding = match &binding {
                Some(binding @ crate::providers::PhaseBinding::Connection { .. }) => {
                    Some(binding.clone())
                }
                _ => None,
            };
            let local_binding_model = match &binding {
                Some(crate::providers::PhaseBinding::Local { model_id }) => Some(model_id.clone()),
                _ => None,
            };
            let llm = if cloud_binding.is_some() {
                None
            } else if teacher || narrator_llm_enabled(&ctx.pool).await? {
                ctx.emit_progress(0.0, None, "loading the language model");
                self.prepare_llm(ctx, &record, local_binding_model.as_deref(), teacher)
                    .await?
            } else {
                tracing::info!(
                    "narrator normalization uses rules; set {NARRATOR_LLM_NORMALIZE_SETTING}=true to use an llm"
                );
                None
            };

            let (port, token) = match await_connection(&*self.sidecar, &ctx.control).await? {
                ConnectionWait::Ready(port, token) => (port, token),
                ConnectionWait::Interrupted(outcome) => return Ok(outcome),
            };

            let scoped: std::collections::HashSet<i64> =
                scoped_pages(&scope, &chapters, book.page_count)
                    .into_iter()
                    .collect();
            let figure_captions: Vec<serde_json::Value> =
                book::list_figures_for_book(&ctx.pool, &record.book_id)
                    .await?
                    .iter()
                    .filter(|figure| scoped.contains(&figure.page_no))
                    .filter_map(figure_caption_body)
                    .collect();

            let book = match ensure_canonical_blocks(
                &ctx.pool,
                &self.client,
                port,
                &token,
                book,
                &self.models_root,
                &crate::ingest_fallback::FallbackSource::from_optional(self.keys.clone()),
                ctx,
            )
            .await?
            {
                Ok(book) => book,
                Err(outcome) => return Ok(outcome),
            };
            let chapters = book::list_chapters(&ctx.pool, &record.book_id).await?;
            let scope_body = build_scope_body(&scope, &chapters);
            let expected_generation =
                crate::page_render::engine_extract_version(&self.client, port, &token).await;
            let pages = canonical_pages(&ctx.pool, &book, expected_generation.as_deref()).await?;
            let section_bounds: Vec<(i64, i64)> = chapters
                .iter()
                .filter(|chapter| !chapter.is_pseudo)
                .map(|chapter| (chapter.page_start, chapter.page_end))
                .collect();
            let mut body = json!({
                "scope": scope_body,
                "mode": record.mode,
                "figure_captions": figure_captions,
                "pages": pages,
                "section_bounds": section_bounds,
            });

            if let Some(cloud_binding) = &cloud_binding {
                let run = super::scripting_cloud::run_cloud_script(
                    ctx,
                    &self.client,
                    &*self.sidecar,
                    self.keys.as_ref(),
                    cloud_binding,
                    &record.mode,
                    body,
                    self.patience_config.clone(),
                )
                .await?;
                return match run {
                    super::scripting_cloud::CloudScriptRun::Interrupted(outcome) => Ok(outcome),
                    super::scripting_cloud::CloudScriptRun::RateLimited(reason) => {
                        ctx.checkpoint.paused_reason = Some(reason.clone());
                        ctx.save_checkpoint().await?;
                        let _ = ctx
                            .merge_report_value(
                                "paused_reason",
                                serde_json::to_value(&reason).unwrap_or_default(),
                            )
                            .await;
                        Ok(StageOutcome::Paused)
                    }
                    super::scripting_cloud::CloudScriptRun::Completed(output) => {
                        if ctx.checkpoint.paused_reason.take().is_some() {
                            ctx.save_checkpoint().await?;
                        }
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
                        if let Some(done_chunks) = output.done_chunks {
                            ctx.checkpoint.done_chunks = done_chunks;
                        }
                        let raw: Vec<RawSegment> = output
                            .segments
                            .into_iter()
                            .map(serde_json::from_value)
                            .collect::<Result<_, _>>()?;
                        persist_script_output(
                            ctx,
                            &chapters,
                            raw,
                            Vec::new(),
                            output.markers_dropped,
                            output.markers_omitted,
                            stage_started,
                        )
                        .await
                    }
                };
            }

            let loaded_model = if let Some((model_id, plan, model_path)) = &llm {
                let plan = clamp_plan_to_runtime(&self.client, port, &token, plan.clone()).await;
                let ram_probe = residency_check::begin_ram_plan(&plan).await;
                load_llm(&self.client, port, &token, model_path, &plan).await?;
                if let Some(probe) = ram_probe {
                    let after_load_free = hardware::probe_ram_free_mb().await;
                    if let Err(error) = residency_check::record_stage_ram_plan(
                        &ctx.pool,
                        &ctx.job_id,
                        JobState::Scripting,
                        probe,
                        after_load_free,
                    )
                    .await
                    {
                        tracing::warn!(
                            "ram residency telemetry failed for job {} stage SCRIPTING: {error}",
                            ctx.job_id
                        );
                    }
                }
                body["model_id"] = json!(model_id);
                Some(model_id.clone())
            } else {
                None
            };

            let mut raw_segments: Option<Vec<RawSegment>> = None;
            let mut raw_suppressed: Vec<RawSuppressed> = Vec::new();
            let mut markers_dropped: u64 = 0;
            let mut markers_omitted: u64 = 0;
            let mut cancelled = false;
            let mut sidecar_error: Option<OrchestratorError> = None;
            let mut chunk_progress: Option<(i64, i64)> = None;
            let run_id = request_run_id(&ctx.job_id);
            let mut control = ctx.control.clone();
            let stream_result = post_sse_cancellable(
                SidecarTarget::new(&self.client, port, &token),
                "/script",
                &run_id,
                body,
                &mut control,
                |event: SseEvent| match event.event.as_str() {
                    "segments" => {
                        if let Some(value) = event.data.get("segments") {
                            raw_segments =
                                serde_json::from_value::<Vec<RawSegment>>(value.clone()).ok();
                        }
                        if let Some(value) = event.data.get("suppressed") {
                            raw_suppressed =
                                serde_json::from_value::<Vec<RawSuppressed>>(value.clone())
                                    .unwrap_or_else(|err| {
                                        tracing::warn!(
                                            "suppressed records dropped: the /script payload did not parse: {err}"
                                        );
                                        Vec::new()
                                    });
                        }
                        markers_dropped = event
                            .data
                            .get("figure_markers_dropped")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        markers_omitted = event
                            .data
                            .get("figure_markers_omitted")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                    }
                    "progress" => {
                        if let (Some(chunk), Some(total)) = (
                            event.data.get("chunk").and_then(|v| v.as_i64()),
                            event.data.get("total").and_then(|v| v.as_i64()),
                        ) {
                            chunk_progress = Some((chunk, total));
                            tracing::debug!(
                                job = %ctx.job_id,
                                "script chunk done: chunk={chunk}/{total}"
                            );
                            if total > 0 {
                                ctx.emit_progress(
                                    chunk as f64 / total as f64,
                                    None,
                                    &format!("chunk {chunk}/{total}"),
                                );
                            }
                        }
                    }
                    "cancelled" => cancelled = true,
                    "error" => {
                        sidecar_error = Some(OrchestratorError::from_engine_event(
                            &event.data,
                            "script failed",
                        ));
                    }
                    _ => {}
                },
            )
            .await;
            let unload_result = match &loaded_model {
                Some(model_id) => {
                    unload_llm_draining(ctx, &self.client, port, &token, model_id, self.drain_delay)
                        .await
                }
                None => Ok(()),
            };
            if let Err(err) = &unload_result {
                tracing::warn!(job = %ctx.job_id, "scripting llm unload failed: {err}");
            }
            if let Some((done_chunks, _)) = chunk_progress {
                ctx.checkpoint.done_chunks = done_chunks;
                ctx.save_checkpoint().await?;
            }
            let stream_outcome = stream_result.map_err(OrchestratorError::from)?;
            if let StreamOutcome::Interrupted(outcome) = stream_outcome {
                tracing::info!(job = %ctx.job_id, "script stream interrupted by pause or cancel");
                return Ok(outcome);
            }
            if sidecar_error.is_none() {
                unload_result?;
            }

            if let Some(outcome) = ctx.control.interruption() {
                return Ok(outcome);
            }
            if cancelled {
                return Ok(StageOutcome::Cancelled);
            }
            if let Some(error) = sidecar_error {
                return Err(error);
            }

            let raw = raw_segments
                .ok_or_else(|| OrchestratorError::Sidecar("script produced no segments".into()))?;

            persist_script_output(
                ctx,
                &chapters,
                raw,
                raw_suppressed,
                markers_dropped,
                markers_omitted,
                stage_started,
            )
            .await
        })
    }
}

/// The shared persistence tail of both script paths: map raw segments onto
/// chapters, store segments and suppressed blocks, merge report counts, and
/// close the checkpoint. The local `/script` SSE path and the S-1d cloud
/// binding path produce byte-identical raw shapes, so everything from here
/// down cannot diverge between them.
async fn persist_script_output(
    ctx: &mut StageContext,
    chapters: &[book::Chapter],
    raw: Vec<RawSegment>,
    raw_suppressed: Vec<RawSuppressed>,
    markers_dropped: u64,
    markers_omitted: u64,
    stage_started: std::time::Instant,
) -> Result<StageOutcome, OrchestratorError> {
    {
        let chapter_ids: Vec<String> = chapters.iter().map(|c| c.id.clone()).collect();
        let fallback_chapter = chapter_ids
            .first()
            .cloned()
            .ok_or_else(|| OrchestratorError::InvalidRequest("book has no chapters".into()))?;

        let segments: Vec<job::ScriptSegment> = raw
            .into_iter()
            .enumerate()
            .map(|(idx, seg)| {
                let chapter_id = chapters
                    .iter()
                    .find(|c| c.idx == seg.chapter_idx)
                    .map(|c| c.id.clone())
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            "segment {} has unmatched chapter_idx {}, falling back to chapter {}",
                            idx,
                            seg.chapter_idx,
                            fallback_chapter
                        );
                        fallback_chapter.clone()
                    });
                let flags_json = if seg.flags.is_empty() {
                    None
                } else {
                    serde_json::to_string(&seg.flags).ok()
                };
                let source_span_json = if seg.source_span.is_null() {
                    None
                } else {
                    serde_json::to_string(&seg.source_span).ok()
                };
                job::ScriptSegment {
                    id: new_id(),
                    job_id: ctx.job_id.clone(),
                    chapter_id,
                    idx: idx as i64,
                    page_no: seg.page_no,
                    kind: seg.kind,
                    figure_id: seg.figure_id,
                    text: seg.text,
                    source_span_json,
                    flags_json,
                    audio_hash: None,
                    duration_ms: None,
                    original_text: None,
                }
            })
            .collect();

        job::insert_segments(&ctx.pool, &segments).await?;
        let suppressed_blocks: Vec<job::SuppressedBlock> = raw_suppressed
            .into_iter()
            .map(|record| job::SuppressedBlock {
                id: new_id(),
                job_id: ctx.job_id.clone(),
                page_no: record.page_no,
                role: record.role,
                reason: record.reason,
                certainty: record.certainty,
                text: record.text,
                words: record.words,
            })
            .collect();
        job::delete_suppressed_blocks(&ctx.pool, &ctx.job_id).await?;
        job::insert_suppressed_blocks(&ctx.pool, &suppressed_blocks).await?;
        tracing::info!(
            job = %ctx.job_id,
            "script summary: segments={} markers_dropped={markers_dropped} \
             markers_omitted={markers_omitted} suppressed={}",
            segments.len(),
            suppressed_blocks.len()
        );
        ctx.merge_report_count("figure_markers_dropped", markers_dropped)
            .await?;
        ctx.merge_report_count("figure_markers_omitted", markers_omitted)
            .await?;
        ctx.checkpoint.total_segments = segments.len() as i64;
        ctx.checkpoint.done_segments = segments.len() as i64;
        ctx.save_checkpoint().await?;
        ctx.emit_progress(1.0, Some(0), &format!("{} segments", segments.len()));
        ctx.merge_report_duration("script", stage_started.elapsed().as_secs())
            .await?;
        Ok(StageOutcome::Completed)
    }
}

fn build_scope_body(scope: &Scope, chapters: &[book::Chapter]) -> serde_json::Value {
    match scope {
        Scope::Pages {
            page_start,
            page_end,
        } => json!({
            "kind": "pages",
            "page_start": page_start,
            "page_end": page_end,
        }),
        Scope::Chapters { chapter_ids } => {
            let bounds: Vec<ChapterBound> = chapters
                .iter()
                .filter(|c| chapter_ids.contains(&c.id))
                .map(|c| ChapterBound {
                    idx: c.idx,
                    page_start: c.page_start,
                    page_end: c.page_end,
                })
                .collect();
            json!({
                "kind": "chapters",
                "chapter_page_bounds": bounds,
            })
        }
    }
}

#[cfg(test)]
mod await_connection_tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::orchestrator::stage::control_channel;

    struct ReadyAfter {
        remaining: AtomicU32,
    }

    impl SidecarProvider for ReadyAfter {
        fn connection(&self) -> Option<(u16, String)> {
            if self.remaining.load(Ordering::SeqCst) == 0 {
                return Some((4242, "tok".to_string()));
            }
            self.remaining.fetch_sub(1, Ordering::SeqCst);
            None
        }
    }

    struct NeverReady;

    impl SidecarProvider for NeverReady {
        fn connection(&self) -> Option<(u16, String)> {
            None
        }
    }

    #[tokio::test(start_paused = true)]
    async fn waits_until_sidecar_becomes_ready() {
        let provider = ReadyAfter {
            remaining: AtomicU32::new(5),
        };
        let (_handle, control) = control_channel();
        let result = await_connection(&provider, &control).await.expect("ready");
        match result {
            ConnectionWait::Ready(port, token) => {
                assert_eq!(port, 4242);
                assert_eq!(token, "tok");
            }
            ConnectionWait::Interrupted(_) => panic!("unexpected interruption"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_when_sidecar_never_ready() {
        let (_handle, control) = control_channel();
        let result = await_connection(&NeverReady, &control).await;
        assert!(matches!(result, Err(OrchestratorError::SidecarUnavailable)));
    }

    #[tokio::test(start_paused = true)]
    async fn pause_during_wait_returns_paused_outcome() {
        let (handle, control) = control_channel();
        handle.pause();
        let result = await_connection(&NeverReady, &control).await.expect("ok");
        assert!(matches!(
            result,
            ConnectionWait::Interrupted(StageOutcome::Paused)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_wait_returns_cancelled_outcome() {
        let (handle, control) = control_channel();
        handle.cancel();
        let result = await_connection(&NeverReady, &control).await.expect("ok");
        assert!(matches!(
            result,
            ConnectionWait::Interrupted(StageOutcome::Cancelled)
        ));
    }

    async fn spawn_health_server(body: &'static str) -> u16 {
        spawn_health_sequence(vec![body]).await
    }

    async fn spawn_health_sequence(bodies: Vec<&'static str>) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut served = 0usize;
            while let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let body = bodies[served.min(bodies.len() - 1)];
                served += 1;
                let _ = crate::test_support::mock_http::write_response(
                    &mut sock,
                    "200 OK",
                    "application/json",
                    body.as_bytes(),
                )
                .await;
            }
        });
        port
    }

    #[tokio::test]
    async fn clamp_forces_cpu_when_runtime_cannot_offload() {
        let port = spawn_health_server(r#"{"status":"ok","gpu_offload":false}"#).await;
        let client = reqwest::Client::new();
        let plan = serde_json::json!({
            "device": "cuda",
            "n_gpu_layers": 12,
            "kv_offload": true,
            "badge": "yellow",
            "tok_s_est": 31.0
        });
        let clamped = clamp_plan_to_runtime(&client, port, "tok", plan).await;
        assert_eq!(clamped["device"]["backend"], "cpu");
        assert_eq!(clamped["n_gpu_layers"], 0);
        assert_eq!(clamped["kv_offload"], false);
        assert_eq!(clamped["badge"], "orange");
        assert!(clamped.get("tok_s_est").is_none());
    }

    #[tokio::test]
    async fn clamp_forces_cpu_for_indexed_gpu_devices() {
        let port = spawn_health_server(r#"{"status":"ok","gpu_offload":false}"#).await;
        let client = reqwest::Client::new();
        let plan = serde_json::json!({"device":"cuda:0","n_gpu_layers":12});
        let clamped = clamp_plan_to_runtime(&client, port, "tok", plan).await;
        assert_eq!(clamped["device"]["backend"], "cpu");
        assert_eq!(clamped["n_gpu_layers"], 0);
    }

    #[tokio::test]
    async fn clamp_keeps_gpu_plan_when_runtime_offloads() {
        let port = spawn_health_server(r#"{"status":"ok","gpu_offload":true}"#).await;
        let client = reqwest::Client::new();
        let plan = serde_json::json!({"device":"cuda","n_gpu_layers":12});
        let clamped = clamp_plan_to_runtime(&client, port, "tok", plan.clone()).await;
        assert_eq!(clamped, plan);
    }

    #[tokio::test]
    async fn clamp_keeps_gpu_plan_when_health_lacks_field() {
        let port = spawn_health_server(r#"{"status":"ok","version":"0.1"}"#).await;
        let client = reqwest::Client::new();
        let plan = serde_json::json!({"device":"cuda","n_gpu_layers":12});
        let clamped = clamp_plan_to_runtime(&client, port, "tok", plan.clone()).await;
        assert_eq!(clamped, plan);
    }

    #[tokio::test]
    async fn clamp_ignores_cpu_plans() {
        let client = reqwest::Client::new();
        let plan = serde_json::json!({"device":"cpu"});
        let clamped = clamp_plan_to_runtime(&client, 1, "tok", plan.clone()).await;
        assert_eq!(clamped, plan);
    }

    #[tokio::test]
    async fn capability_clamp_forces_cpu_when_torch_lacks_cuda() {
        let port =
            spawn_health_server(r#"{"status":"ok","gpu_offload":true,"torch_cuda":false}"#).await;
        let client = reqwest::Client::new();
        let plan = serde_json::json!({
            "device": "cuda",
            "n_gpu_layers": 43,
            "badge": "yellow",
            "tok_s_est": 55.0
        });
        let clamped =
            clamp_plan_to_capability(&client, port, "tok", plan, "torch_cuda", false).await;
        assert_eq!(clamped["device"]["backend"], "cpu");
        assert_eq!(clamped["badge"], "orange");
        assert!(clamped.get("tok_s_est").is_none());
    }

    #[tokio::test]
    async fn capability_clamp_fails_closed_when_field_missing() {
        let port = spawn_health_server(r#"{"status":"ok","gpu_offload":true}"#).await;
        let client = reqwest::Client::new();
        let plan = serde_json::json!({"device":"cuda","n_gpu_layers":43});
        let clamped = clamp_plan_to_capability(&client, port, "tok", plan, "ct2_cuda", false).await;
        assert_eq!(clamped["device"]["backend"], "cpu");
    }

    #[tokio::test]
    async fn capability_clamp_fail_open_keeps_plan_when_field_missing() {
        let port = spawn_health_server(r#"{"status":"ok"}"#).await;
        let client = reqwest::Client::new();
        let plan = serde_json::json!({"device":"cuda","n_gpu_layers":43});
        let clamped =
            clamp_plan_to_capability(&client, port, "tok", plan.clone(), "gpu_offload", true).await;
        assert_eq!(clamped, plan);
    }

    /// A CPU-only profile with `free_mb` free. `total_mb` is twice free so the
    /// capacity term is never the binding one and these fixtures stay about the
    /// term they name.
    fn cpu_profile(free_mb: u64) -> crate::hardware::HardwareProfile {
        use crate::hardware::{CpuInfo, DiskInfo, HardwareProfile, RamInfo};
        HardwareProfile {
            devices: Vec::new(),
            ram: RamInfo {
                total_mb: free_mb * 2,
                free_mb,
                cgroup: Default::default(),
            },
            cpu: CpuInfo {
                cores_physical: 8,
                cores_logical: 16,
                avx2: true,
                avx512: false,
            },
            disk: DiskInfo {
                models_free_gb: 10.0,
                cache_free_gb: 10.0,
            },
        }
    }

    fn llm_entry() -> catalog::CatalogEntry {
        // The opening brace sits on its own line, not on the `r#"`. That is not
        // cosmetic: `hardware::device`'s plan-device tripwire counts braces with
        // a `strip_literals` that carries no state across lines, so a multi-line
        // raw string is mis-counted and the `#[cfg(test)] mod tests` window it
        // depends on closes early. Every test line after the close is then
        // classified as production.
        //
        // Three files trip it on `origin/main`, with two distinct triggers:
        // `r#"{` here and in `reading/anchor.rs` (net -1, losing anchor.rs from
        // line 978), and `format!`'s doubled `r#"{{` in `catalog/mod.rs` (net
        // -2, closing that window before its first test and misclassifying the
        // whole module). Filed rather than folded in — it is another guard's
        // bug, and fixing it is a cross-line literal state machine with its own
        // regression surface around char literals and lifetimes.
        serde_json::from_str(
            r#"
            {
                "id": "mllm-test",
                "task": "llm",
                "display_name": "Test Model",
                "family": "test",
                "quant": "Q4_K_M",
                "format": "gguf",
                "disk_bytes": 1000,
                "full_gpu_vram_mb": 2000,
                "min_free_ram_mb": 4096,
                "hybrid_capable": true,
                "ctx_max": 8192,
                "quality_score": 80,
                "speed_score": 70,
                "capabilities": ["teacher_mode"],
                "languages": ["en"],
                "license": { "spdx": "Apache-2.0", "commercial_ok": true },
                "download": { "files": [{ "url": "https://example.com/m.gguf", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "dest_path": "m.gguf", "size_bytes": 1000 }] },
                "source": "curated",
                "evidence": ["https://example.com"]
            }
            "#,
        )
        .expect("entry json")
    }

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn no_fit_fallback_is_runnable_but_red() {
        let entry = llm_entry();
        let info = catalog::gguf::GgufModelInfo {
            n_layers: 10,
            layer_bytes: vec![100 * MIB; 10],
            non_layer_bytes: 50 * MIB,
            output_bytes: 100 * MIB,
            kv_bytes_per_token: 0,
            n_vocab: 0,
        };
        // 1050 MiB of weights plus a 448 MiB compute buffer is 1498 MiB, so
        // "roomy" needs a gate above that. This said 1500 MB free before R-AM (a
        // 1200 MiB gate, which the old weights-only arithmetic accepted and the
        // whole charge does not) and 2048 MB between R-AM's two gate changes (a
        // 1024 MiB gate once the absolute OS reserve joined the min). 2560 MB
        // free gives a 1536 MiB gate. Starving it by 945 MB leaves 1615 MB free,
        // a 591 MiB gate, which is Red as the second half of the test wants.
        // Both flips are pinned in `hardware::plan::tests`; the figure moves
        // here so the fixture keeps expressing its own intent rather than
        // accidentally testing the boundary.
        let mut profile = cpu_profile(2_560);
        let roomy = plan_from_profile(&entry, &profile, Some(&info), "mllm-test");
        assert_eq!(roomy["device"]["backend"], "cpu");
        assert_ne!(roomy["badge"], "red");

        hardware::reserve_memory(&mut profile, 945);
        let starved = plan_from_profile(&entry, &profile, Some(&info), "mllm-test");
        assert_eq!(starved["device"]["backend"], "cpu");
        assert_eq!(starved["badge"], "red");
        assert_eq!(starved["n_gpu_layers"], 0);
        assert!(starved["n_layers_total"].as_u64().unwrap() >= 1);
        assert!(starved["ctx"].as_u64().unwrap() >= 512);
        assert!(starved["batch"].as_u64().unwrap() >= 1);
    }

    /// Recorded badge change, the relaxing direction.
    ///
    /// The planner walks its ctx ladder at batch 512 and refuses. Before R-AM
    /// the `plan: None` arm stamped `red` over whatever `fallback_cpu_plan`
    /// returned, so this machine got a red badge — and a spurious 🔴 confirm
    /// dialog — on a plan that fits with room to spare, because the fallback
    /// runs at batch 256 and nothing had ever charged it.
    ///
    /// Arithmetic (1000 MiB weights, vocab 151_936, no KV):
    ///   planner @ batch 512 = 1000 + 448 + 296.75 = 1744.75 MiB  > 1676 gate
    ///   fallback @ batch 256 = 1000 + 448 + 148.38 = 1596.38 MiB  ≤ 1676 gate
    ///
    /// 2700 MB free is below 5120, so the absolute OS reserve is the binding
    /// term and the gate is `2700 - 1024`. The figure was 2048 MB when the gate
    /// was `0.8 * free`.
    ///
    /// Both directions matter and this is the one that is easy to leave broken:
    /// a red badge that no number supports makes red mean nothing, which is the
    /// same defect as an unexamined plan wearing orange.
    #[test]
    fn a_refused_plan_whose_fallback_fits_is_no_longer_stamped_red() {
        let entry = llm_entry();
        let info = catalog::gguf::GgufModelInfo {
            n_layers: 10,
            layer_bytes: vec![90 * MIB; 10],
            non_layer_bytes: 100 * MIB,
            output_bytes: 50 * MIB,
            kv_bytes_per_token: 0,
            n_vocab: 151_936,
        };
        let profile = cpu_profile(2_700);
        let gate = hardware::plan::ram_gate_mb(&profile.ram);
        assert_eq!(gate, 1_676);

        // Premises, so the fixture cannot drift into testing a different rung.
        let planner = hardware::plan::cpu_ram_charge(&info, 4_096, 512).total();
        let fallback = hardware::plan::cpu_ram_charge(&info, 4_096, 256).total();
        assert!(
            !hardware::plan::fits_ram(planner, gate),
            "premise: the planner must refuse at batch 512"
        );
        assert!(
            hardware::plan::fits_ram(fallback, gate),
            "premise: the fallback must fit at batch 256"
        );

        let fit = hardware::plan::compute_fit(&entry, &profile, Some(&info));
        assert!(fit.plan.is_none(), "premise: the ladder emits nothing");

        let plan = plan_from_profile(&entry, &profile, Some(&info), "mllm-test");
        assert_eq!(
            plan["badge"], "orange",
            "the emitted plan fits; red was a lie"
        );
        assert_eq!(plan["batch"], 256);
        assert_eq!(plan["device"]["backend"], "cpu");
    }

    /// TASKS.md R-AM's closing box: every gate this task added shrinks or
    /// warns — none may decline. The grid crosses the bundled catalog with
    /// machines from roomy down to below the OS reserve (gate 0), with and
    /// without a readable GGUF header, on both the scripting and the vision
    /// planning surfaces. Every cell must emit a runnable plan wearing an
    /// advisory badge; a single refusal anywhere is a model selection made
    /// unavailable by a memory figure, which ARCHITECTURE §6.1 invariant 1
    /// and AGENTS §5a forbid outright.
    #[test]
    fn no_selection_becomes_unavailable_under_any_gate_this_task_added() {
        let info = catalog::gguf::GgufModelInfo {
            n_layers: 10,
            layer_bytes: vec![90 * MIB; 10],
            non_layer_bytes: 100 * MIB,
            output_bytes: 50 * MIB,
            kv_bytes_per_token: 32_768,
            n_vocab: 151_936,
        };
        let free_mbs = [512u64, 1_024, 2_048, 2_700, 8_192];
        assert!(
            hardware::plan::ram_gate_mb(&cpu_profile(512).ram) == 0,
            "premise: the grid must include the clamped-to-zero gate, rung 3's limit case"
        );

        let llm_entries: Vec<_> = bundled_catalog()
            .models
            .iter()
            .filter(|e| e.task == catalog::ModelTask::Llm)
            .collect();
        assert!(
            llm_entries.len() >= 2,
            "premise: the grid covers the catalog, not one hand-picked entry"
        );

        let mut cells = 0;
        let mut assert_runnable = |plan: serde_json::Value, site: &str, id: &str, free: u64| {
            let badge = plan["badge"].as_str().unwrap_or_default().to_string();
            assert!(
                ["green", "yellow", "orange", "red"].contains(&badge.as_str()),
                "{site} for {id} at {free} MB free emitted badge {badge:?} — \
                 the badge is advisory and must always exist"
            );
            assert!(
                crate::hardware::device::plan_device(&plan).is_ok(),
                "{site} for {id} at {free} MB free emitted no runnable device — a refusal"
            );
            assert!(plan["ctx"].as_u64().unwrap_or(0) >= 512);
            assert!(plan["batch"].as_u64().unwrap_or(0) >= 1);
            cells += 1;
        };

        for entry in &llm_entries {
            for &free in &free_mbs {
                let profile = cpu_profile(free);
                for gguf in [None, Some(&info)] {
                    assert_runnable(
                        plan_from_profile(entry, &profile, gguf, &entry.id),
                        "plan_from_profile",
                        &entry.id,
                        free,
                    );
                    assert_runnable(
                        plan_json_from_fit(
                            hardware::plan::vision_fit(entry, &profile, gguf, 990_372_672),
                            &entry.id,
                            gguf,
                            &profile.ram,
                        ),
                        "vision_fit",
                        &entry.id,
                        free,
                    );
                    assert_runnable(
                        plan_json_from_fit(
                            hardware::plan::vision_cpu_fit(entry, &profile, gguf, 990_372_672),
                            &entry.id,
                            gguf,
                            &profile.ram,
                        ),
                        "vision_cpu_fit",
                        &entry.id,
                        free,
                    );
                }
            }
        }
        assert!(cells >= 60, "the grid ran, {cells} cells");
    }

    #[tokio::test]
    async fn capability_probe_polls_through_pending_null() {
        let port = spawn_health_sequence(vec![
            r#"{"status":"ok","torch_cuda":null}"#,
            r#"{"status":"ok","torch_cuda":null}"#,
            r#"{"status":"ok","torch_cuda":true}"#,
        ])
        .await;
        let client = reqwest::Client::new();
        let plan = serde_json::json!({"device":"cuda","n_gpu_layers":43});
        let clamped =
            clamp_plan_to_capability(&client, port, "tok", plan.clone(), "torch_cuda", false).await;
        assert_eq!(clamped, plan);
    }
}

#[cfg(test)]
mod blocks_line_tests {
    use super::*;

    const CURRENT: &str =
        "stage SCRIPTING blocks: pages=923 flagged=4 version=v1|extract=3|roles=1 \
generation=current";

    #[test]
    fn the_blocks_line_names_the_generation_it_is_reading() {
        assert_eq!(
            format_blocks_line(923, 4, Some("v1|extract=3|roles=1"), true),
            CURRENT
        );
    }

    #[test]
    fn a_book_left_on_an_older_extractor_says_so_in_the_log() {
        let line = format_blocks_line(923, 4, Some("v1|extract=2|roles=1"), false);
        assert!(
            line.ends_with("generation=stale"),
            "a re-create that still sounds old has to be attributable without re-running \
             anything: {line}"
        );
    }
}
