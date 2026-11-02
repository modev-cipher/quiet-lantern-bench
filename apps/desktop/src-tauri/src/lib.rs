#[cfg(feature = "book-agent")]
pub mod agent_ipc;
#[cfg(feature = "book-agent")]
pub mod agent_session;
#[cfg(all(test, feature = "book-agent"))]
mod agent_smoke;
pub mod cache_gc;
pub mod cache_path;
pub mod catalog;
pub mod diagnostics;
pub mod engine_install;
pub mod hardware;
pub mod ingest_fallback;
pub mod lan_sender;
pub mod learning_profile;
pub mod library;
pub mod logging;
pub mod media_server;
pub mod model_manager;
pub mod model_pins;
mod mp3_probe;
pub mod orchestrator;
pub mod page_peek;
pub mod page_render;
pub mod process_group;
pub mod process_priority;
pub mod providers;
pub mod reading;
#[cfg(debug_assertions)]
pub mod reading_fixture;
pub mod render;
#[cfg(feature = "book-agent")]
pub mod retrieval;
pub mod sidecar;
pub mod startup;
pub mod storage;
#[cfg(test)]
mod test_support;
pub mod updater;
pub mod voice;
pub mod voice_capture;
pub mod voice_journal;
pub mod voice_playback;
pub mod voice_presentation;
pub mod voice_sample;
pub mod voice_session;
pub mod voices;
pub mod window_chrome;
pub mod words;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};
use tokio::sync::{mpsc, watch};

use catalog::Catalog;
use model_manager::{DownloadProgress, ModelManager};
use orchestrator::{
    AligningStage, CreateJobRequest, JobOrchestrator, JobState, LayoutStage, RenderingStage, Scope,
    ScriptingStage, SegmentEdit, SidecarProvider, StageRegistry, SynthesizingStage, VisionStage,
};
use sidecar::protocol::EngineStatus;
use sidecar::supervisor::{self, SupervisorHandle};
use storage::models::ModelInstall;

struct AppSidecarProvider {
    app: tauri::AppHandle,
}

struct ShutdownCoordinator {
    state: std::sync::atomic::AtomicU8,
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self {
            state: std::sync::atomic::AtomicU8::new(0),
        }
    }
}

impl ShutdownCoordinator {
    fn begin(&self) -> bool {
        self.state
            .compare_exchange(
                0,
                1,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    fn finish(&self) {
        self.state.store(2, std::sync::atomic::Ordering::Release);
    }

    fn finished(&self) -> bool {
        self.state.load(std::sync::atomic::Ordering::Acquire) == 2
    }
}

pub fn stop_lan_sender<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(active) = app
        .state::<lan_sender::session::LanSenderHandle>()
        .active()
        .take()
    {
        active.server.stop();
    }
}

async fn shutdown_app(app: tauri::AppHandle) {
    if app.try_state::<startup::StartupFailure>().is_some() {
        app.state::<ShutdownCoordinator>().finish();
        app.exit(0);
        return;
    }
    stop_lan_sender(&app);
    let orchestrator = app.state::<Arc<JobOrchestrator>>().inner().clone();
    let supervisor = app.state::<SupervisorHandle>();
    orchestrator.begin_shutdown().await;
    let deadline = std::time::Duration::from_secs(10);
    let shutdown = async {
        supervisor.request_engine_shutdown().await;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        supervisor.begin_shutdown();
        if let Err(error) = orchestrator
            .shutdown(std::time::Duration::from_secs(5))
            .await
        {
            tracing::warn!("job shutdown failed: {error}");
        }
        supervisor
            .shutdown(std::time::Duration::from_millis(3500))
            .await;
    };
    let _ = tokio::time::timeout(deadline, shutdown).await;
    app.state::<ShutdownCoordinator>().finish();
    app.exit(0);
}

fn request_shutdown(app: &tauri::AppHandle) {
    let coordinator = app.state::<ShutdownCoordinator>();
    if !coordinator.begin() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        shutdown_app(app).await;
    });
}

impl SidecarProvider for AppSidecarProvider {
    fn connection(&self) -> Option<(u16, String)> {
        self.app.state::<SupervisorHandle>().connection()
    }

    fn respawn(&self) {
        self.app.state::<SupervisorHandle>().respawn();
    }
}

#[derive(Default)]
pub struct DownloadRegistry(Mutex<HashMap<String, watch::Sender<bool>>>);

#[derive(Debug, Clone, serde::Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum DownloadResult {
    Installed { model_id: String },
    Failed { model_id: String, error: String },
}

#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogModelView {
    #[serde(flatten)]
    pub entry: catalog::CatalogEntry,
    pub badge: hardware::plan::FitBadge,
    pub memory_unreadable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision_badge: Option<hardware::plan::FitBadge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projector_bytes: Option<u64>,
    pub manifest_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<hardware::plan::ResourcePlan>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CatalogView {
    pub catalog_version: String,
    pub models: Vec<CatalogModelView>,
}

fn installed_gguf_path(
    entry: &catalog::CatalogEntry,
    installs: &[ModelInstall],
    models_root: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if entry.format != catalog::ModelFormat::Gguf {
        return None;
    }
    let install = installs.iter().find(|i| i.model_id == entry.id)?;
    let file = entry
        .download
        .files
        .iter()
        .find(|f| f.dest_path.ends_with(".gguf"))?;
    Some(models_root.join(&install.path).join(&file.dest_path))
}

fn build_catalog_view(
    catalog: &Catalog,
    profile: &hardware::HardwareProfile,
    installs: &[ModelInstall],
    models_root: &std::path::Path,
) -> CatalogView {
    let models = catalog
        .models
        .iter()
        .map(|entry| {
            let gguf_info = installed_gguf_path(entry, installs, models_root)
                .and_then(|path| catalog::gguf::read_model_info(&path).ok());
            let fit = hardware::plan::compute_fit(entry, profile, gguf_info.as_ref());
            let vision_badge = orchestrator::vision_model::mmproj_file(entry).map(|mmproj| {
                hardware::plan::vision_fit(entry, profile, gguf_info.as_ref(), mmproj.size_bytes)
                    .badge
            });
            CatalogModelView {
                badge: fit.badge,
                memory_unreadable: hardware::plan::planned_on_unreadable_memory(entry, profile),
                vision_badge,
                projector_bytes: orchestrator::vision_model::mmproj_file(entry)
                    .map(|f| f.size_bytes),
                manifest_sha256: model_manager::manifest_hash(&entry.download.files),
                plan: fit.plan,
                entry: entry.clone(),
            }
        })
        .collect();
    CatalogView {
        catalog_version: catalog.catalog_version.clone(),
        models,
    }
}

#[tauri::command]
async fn get_catalog(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    manager: tauri::State<'_, Arc<ModelManager>>,
) -> Result<CatalogView, String> {
    let profile = detect_profile(&app, &pool).await?;
    let installs = manager.list_installed().await.map_err(|e| e.to_string())?;
    let models_root = manager.models_root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        build_catalog_view(
            catalog::bundled_catalog(),
            &profile,
            &installs,
            &models_root,
        )
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn engine_status(state: tauri::State<'_, SupervisorHandle>) -> EngineStatus {
    state.status()
}

type ProfileCache = Arc<tokio::sync::OnceCell<hardware::HardwareProfile>>;

async fn storage_dirs(
    app: &tauri::AppHandle,
    pool: &storage::SqlitePool,
) -> Result<storage::paths::StorageDirs, String> {
    let data_dir = storage::paths::resolve_app_data_dir(app)?;
    let cache_dir = storage::paths::resolve_app_cache_dir(app)?;
    storage::paths::resolve_dirs(pool, &data_dir, cache_dir)
        .await
        .map_err(|e| e.to_string())
}

async fn detect_profile(
    app: &tauri::AppHandle,
    pool: &storage::SqlitePool,
) -> Result<hardware::HardwareProfile, String> {
    let dirs = storage_dirs(app, pool).await?;
    Ok(hardware::detect(dirs.models_dir(), models_origin(&dirs), &dirs.cache_dir).await)
}

fn models_origin(dirs: &storage::paths::StorageDirs) -> hardware::DirOrigin {
    if dirs.models.is_default() {
        hardware::DirOrigin::Default
    } else {
        hardware::DirOrigin::Configured
    }
}

async fn cached_profile(
    app: &tauri::AppHandle,
    pool: &storage::SqlitePool,
    cache: &ProfileCache,
) -> Result<hardware::HardwareProfile, String> {
    let mut profile = cache
        .get_or_try_init(|| detect_profile(app, pool))
        .await
        .cloned()?;
    let dirs = storage_dirs(app, pool).await?;
    profile.disk = hardware::detect_disk(dirs.models_dir(), models_origin(&dirs), &dirs.cache_dir);
    Ok(profile)
}

fn kokoro_cuda_identity(profile: &hardware::HardwareProfile) -> Option<String> {
    let mut cuda = profile
        .devices
        .iter()
        .filter(|device| device.backend == hardware::DeviceBackend::Cuda);
    match (cuda.next(), cuda.next()) {
        (Some(device), None) => Some(device.key.clone()),
        _ => None,
    }
}

async fn migrate_learned_keys(
    pool: &storage::SqlitePool,
    profile: &hardware::HardwareProfile,
) -> Result<bool, sqlx::Error> {
    let cuda_identity = kokoro_cuda_identity(profile);
    let mut migrated = false;
    storage::models::update_learned(pool, |learned| {
        migrated = learned.migrate_legacy_gpu_keys(cuda_identity.as_deref());
    })
    .await?;
    Ok(migrated)
}

static TORCH_CUDA: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
static EXTRACT_VERSION: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn shared_client() -> &'static reqwest::Client {
    HTTP.get_or_init(reqwest::Client::new)
}

async fn layout_runs_on_gpu(supervisor: &SupervisorHandle) -> bool {
    if let Some(known) = TORCH_CUDA.get() {
        return *known;
    }
    let Some((port, token)) = supervisor.connection() else {
        return false;
    };
    match orchestrator::engine_capability_known(shared_client(), port, &token, "torch_cuda").await {
        Some(answer) => {
            let _ = TORCH_CUDA.set(answer);
            answer
        }
        None => false,
    }
}

/// The extraction identity of the engine this session is talking to, cached once.
///
/// Cached because it is constant for an engine build and this is asked once per library
/// listing. The limitation that follows: a sidecar swapped **mid-session** for one with a
/// different extractor keeps the first answer until the app restarts, so a book would be
/// judged against the retired build. Only a failed probe is left uncached, so an engine that
/// was down at first ask can still answer later — the same shape as `TORCH_CUDA`.
async fn engine_generation(supervisor: &SupervisorHandle) -> Option<String> {
    if let Some(known) = EXTRACT_VERSION.get() {
        return Some(known.clone());
    }
    let (port, token) = supervisor.connection()?;
    let answer = page_render::engine_extract_version(shared_client(), port, &token).await?;
    let _ = EXTRACT_VERSION.set(answer.clone());
    Some(answer)
}

#[tauri::command]
async fn plan_import(
    app: tauri::AppHandle,
    supervisor: tauri::State<'_, SupervisorHandle>,
    manager: tauri::State<'_, Arc<ModelManager>>,
    path: String,
) -> Result<library::ImportPlan, String> {
    let (port, token) = supervisor
        .connection()
        .ok_or("engine is not ready".to_string())?;
    let client = reqwest::Client::new();
    let probe = library::probe_document(&client, port, &token, &path).await?;
    let layout_available = library::docling_artifacts_dir(manager.models_root()).is_some()
        && !engine_install::capability::lite_engine_active(&storage::paths::resolve_app_data_dir(
            &app,
        )?);
    Ok(library::plan_layout(
        probe.page_count,
        probe.reflowed,
        layout_available,
        layout_runs_on_gpu(&supervisor).await,
    ))
}

/// Stop handles for in-flight re-extractions, keyed by book and stamped with a pass id.
///
/// The pass id is what makes eviction safe. Registering before the claim let a caller that
/// went on to lose the claim delete the winner's handle on its way out, leaving a two-hour
/// pass with no stop control; and a plain remove-by-book could delete the *next* pass's
/// handle if one started between release and cleanup.
struct PassEntry {
    handle: orchestrator::stage::ControlHandle,
    page: i64,
    pages: i64,
}

#[derive(Default)]
struct ExtractionControls(std::sync::Mutex<std::collections::HashMap<String, (PassEntry, String)>>);

impl ExtractionControls {
    fn register(&self, book_id: &str, pass_id: &str, handle: orchestrator::stage::ControlHandle) {
        if let Ok(mut registry) = self.0.lock() {
            registry.insert(
                book_id.to_string(),
                (
                    PassEntry {
                        handle,
                        page: 0,
                        pages: 0,
                    },
                    pass_id.to_string(),
                ),
            );
        }
    }

    fn release(&self, book_id: &str, pass_id: &str) {
        if let Ok(mut registry) = self.0.lock() {
            if registry.get(book_id).is_some_and(|(_, id)| id == pass_id) {
                registry.remove(book_id);
            }
        }
    }

    fn handle_for(&self, book_id: &str) -> Option<orchestrator::stage::ControlHandle> {
        self.0
            .lock()
            .ok()
            .and_then(|registry| registry.get(book_id).map(|(entry, _)| entry.handle.clone()))
    }

    fn note_progress(&self, book_id: &str, page: i64, pages: i64) {
        if let Ok(mut registry) = self.0.lock() {
            if let Some((entry, _)) = registry.get_mut(book_id) {
                entry.page = page;
                entry.pages = pages;
            }
        }
    }

    /// The single pass in flight, if any.
    ///
    /// At most one entry can exist: a pass is registered only after `claim_extraction`
    /// succeeds, and the frontend refuses to start a second, so `next()` is not an
    /// arbitrary choice among several. Returning one keeps the restored view honest about
    /// what the backend actually permits.
    fn active(&self) -> Option<ActiveExtraction> {
        self.0.lock().ok().and_then(|registry| {
            registry
                .iter()
                .next()
                .map(|(book_id, (entry, _))| ActiveExtraction {
                    book_id: book_id.clone(),
                    page: entry.page,
                    pages: entry.pages,
                })
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveImport {
    pub import_id: String,
    pub filename: String,
    pub page: i64,
    pub total: i64,
    pub stage: String,
}

struct ImportEntry {
    filename: String,
    page: i64,
    total: i64,
    stage: String,
    control_handle: orchestrator::stage::ControlHandle,
}

#[derive(Default)]
struct ImportControls(std::sync::Mutex<Option<(String, ImportEntry)>>);

impl ImportControls {
    fn register(
        &self,
        import_id: &str,
        filename: &str,
        handle: orchestrator::stage::ControlHandle,
    ) {
        if let Ok(mut registry) = self.0.lock() {
            *registry = Some((
                import_id.to_string(),
                ImportEntry {
                    filename: filename.to_string(),
                    page: 0,
                    total: 0,
                    stage: "extract".to_string(),
                    control_handle: handle,
                },
            ));
        }
    }

    fn release(&self, import_id: &str) {
        if let Ok(mut registry) = self.0.lock() {
            if registry.as_ref().is_some_and(|(id, _)| id == import_id) {
                *registry = None;
            }
        }
    }

    fn note_progress(&self, import_id: &str, page: i64, total: i64, stage: &str) {
        if let Ok(mut registry) = self.0.lock() {
            if let Some((id, entry)) = registry.as_mut() {
                if id == import_id {
                    entry.page = page;
                    entry.total = total;
                    entry.stage = stage.to_string();
                }
            }
        }
    }

    fn active(&self) -> Option<ActiveImport> {
        self.0.lock().ok().and_then(|registry| {
            registry.as_ref().map(|(id, entry)| ActiveImport {
                import_id: id.clone(),
                filename: entry.filename.clone(),
                page: entry.page,
                total: entry.total,
                stage: entry.stage.clone(),
            })
        })
    }

    fn cancel(&self) -> bool {
        if let Ok(registry) = self.0.lock() {
            if let Some((_, entry)) = registry.as_ref() {
                entry.control_handle.cancel();
                return true;
            }
        }
        false
    }
}

/// RAII Scope Guard ensuring active import registration is always released upon drop.
struct ImportScopeGuard {
    controls: Arc<ImportControls>,
    import_id: String,
}

impl Drop for ImportScopeGuard {
    fn drop(&mut self) {
        self.controls.release(&self.import_id);
    }
}

async fn plan_for_book(
    pool: &storage::SqlitePool,
    supervisor: &SupervisorHandle,
    manager: &Arc<ModelManager>,
    book_id: &str,
) -> Result<BookPlan, String> {
    let book = storage::book::get_book(pool, book_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("ERR_NO_BOOK: book '{book_id}' is not in the library"))?;
    let docling_dir = library::docling_artifacts_dir(manager.models_root());
    let plan = library::plan_layout(
        book.page_count.unwrap_or(0),
        book.reflowed,
        docling_dir.is_some(),
        layout_runs_on_gpu(supervisor).await,
    );
    let generation_current = library::generation_is_current(
        book.blocks_version.as_deref(),
        engine_generation(supervisor).await.as_deref(),
    );
    Ok(BookPlan {
        book,
        plan,
        docling_dir,
        generation_current,
    })
}

struct BookPlan {
    book: storage::book::Book,
    plan: library::ImportPlan,
    docling_dir: Option<String>,
    generation_current: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtractionPlan {
    #[serde(flatten)]
    layout: library::ImportPlan,
    blocked: Option<String>,
}

#[tauri::command]
async fn plan_extraction(
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    manager: tauri::State<'_, Arc<ModelManager>>,
    book_id: String,
) -> Result<ExtractionPlan, String> {
    let planned = plan_for_book(&pool, &supervisor, &manager, &book_id).await?;
    let active = storage::book::count_active_jobs(&pool, &book_id)
        .await
        .map_err(|e| e.to_string())?;
    let extracting = storage::book::is_extracting(&pool, &book_id, storage::now_ms())
        .await
        .map_err(|e| e.to_string())?;
    Ok(ExtractionPlan {
        blocked: library::extraction_blocked_reason(
            planned.book.layout_analysed,
            planned.book.is_scanned,
            &planned.plan,
            active,
            extracting,
            planned.generation_current,
        ),
        layout: planned.plan,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ActiveExtraction {
    book_id: String,
    page: i64,
    pages: i64,
}

/// What is running right now, for a view that was not mounted when it started.
///
/// The pass outlives the Library component: navigating away unmounts it and takes the
/// progress row, the Stop button and the one-at-a-time hint with it, while an hours-long
/// operation carries on invisibly. Correctness is already the lease's job; this is so the
/// user does not lose sight of, or control over, work in flight.
#[tauri::command]
async fn active_extraction(
    controls: tauri::State<'_, Arc<ExtractionControls>>,
) -> Result<Option<ActiveExtraction>, String> {
    Ok(controls.inner().active())
}

#[tauri::command]
async fn cancel_extraction(
    controls: tauri::State<'_, Arc<ExtractionControls>>,
    book_id: String,
) -> Result<bool, String> {
    match controls.inner().handle_for(&book_id) {
        Some(handle) => {
            handle.cancel();
            Ok(true)
        }
        None => Ok(false),
    }
}

#[tauri::command]
async fn active_import(
    controls: tauri::State<'_, Arc<ImportControls>>,
) -> Result<Option<ActiveImport>, String> {
    Ok(controls.inner().active())
}

#[tauri::command]
async fn cancel_import(controls: tauri::State<'_, Arc<ImportControls>>) -> Result<bool, String> {
    Ok(controls.inner().cancel())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn improve_extraction(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    manager: tauri::State<'_, Arc<ModelManager>>,
    controls: tauri::State<'_, Arc<ExtractionControls>>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    book_id: String,
    layout: Option<bool>,
) -> Result<library::BackfillOutcome, String> {
    let (port, token) = supervisor
        .connection()
        .ok_or("engine is not ready".to_string())?;
    let planned = plan_for_book(&pool, &supervisor, &manager, &book_id).await?;
    let book = planned.book;
    let docling_dir = planned.docling_dir;
    let active = storage::book::count_active_jobs(&pool, &book_id)
        .await
        .map_err(|e| e.to_string())?;
    let extracting = storage::book::is_extracting(&pool, &book_id, storage::now_ms())
        .await
        .map_err(|e| e.to_string())?;
    if let Some(reason) = library::extraction_blocked_reason(
        book.layout_analysed,
        book.is_scanned,
        &planned.plan,
        active,
        extracting,
        planned.generation_current,
    ) {
        return Err(reason);
    }

    let (handle, mut control) = orchestrator::stage::control_channel();
    let pass_id = storage::new_id();
    let client = reqwest::Client::new();
    let emitter = app.clone();
    let progress_id = book_id.clone();
    let progress_controls = Arc::clone(controls.inner());
    let outcome = library::improve_extraction(
        &pool,
        orchestrator::stream::SidecarTarget::new(&client, port, &token),
        &book,
        library::PassContext {
            docling_dir,
            layout: layout.unwrap_or(true),
            owner: &pass_id,
            control: &mut control,
            lose_handle: Some(handle.clone()),
            fallback: ingest_fallback::FallbackSource::from_manager(Arc::clone(&keys)),
        },
        || {
            controls
                .inner()
                .register(&book_id, &pass_id, handle.clone())
        },
        move |page, pages| {
            progress_controls.note_progress(&progress_id, page, pages);
            let _ = emitter.emit(
                "improve-extraction-progress",
                &serde_json::json!({
                    "bookId": progress_id,
                    "page": page,
                    "pages": pages,
                }),
            );
        },
    )
    .await;

    controls.inner().release(&book_id, &pass_id);
    let _ = app.emit(
        "improve-extraction-finished",
        &serde_json::json!({ "bookId": book_id }),
    );
    outcome
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HardwareProfileView {
    #[serde(flatten)]
    profile: hardware::HardwareProfile,
    primary_key: Option<String>,
}

#[tauri::command]
async fn get_hardware_profile(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    profile_cache: tauri::State<'_, ProfileCache>,
) -> Result<HardwareProfileView, String> {
    let profile = cached_profile(&app, &pool, &profile_cache).await?;
    let primary_key = profile.primary().map(|device| device.key.clone());
    Ok(HardwareProfileView {
        profile,
        primary_key,
    })
}

#[tauri::command]
async fn get_compute_estimate(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<hardware::score::ComputeEstimate, String> {
    let profile = detect_profile(&app, &pool).await?;
    Ok(hardware::score::compute_estimate(
        &profile,
        catalog::bundled_catalog(),
    ))
}

#[tauri::command]
async fn estimate_job_eta(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    profile_cache: tauri::State<'_, ProfileCache>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    request: hardware::eta::EtaRequest,
) -> Result<hardware::eta::EtaBreakdown, String> {
    let learned = storage::models::load_learned(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let catalog = catalog::bundled_catalog();
    let profile = cached_profile(&app, &pool, &profile_cache).await?;
    let static_tok_s = hardware::score::static_tok_s(&profile, catalog);
    let torch_capable = layout_runs_on_gpu(&supervisor).await;
    let devices = hardware::eta::EtaDevices::resolve(&request, &profile, catalog, torch_capable);
    let inputs = hardware::eta::resolve_inputs(&request, &devices, &learned, catalog, static_tok_s);
    Ok(hardware::eta::estimate_eta(&inputs))
}

#[tauri::command]
async fn render_pages(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    book_id: String,
    pages: Vec<i64>,
) -> Result<Vec<page_render::PageRender>, String> {
    let (port, token) = supervisor
        .connection()
        .ok_or("engine is not ready".to_string())?;
    let book = storage::book::get_book(&pool, &book_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("book '{book_id}' not found"))?;
    let cache_dir = storage::paths::resolve_app_cache_dir(&app)?;
    let request = page_render::RenderPagesRequest {
        book_path: book.source_path,
        content_hash: book.content_hash,
        pages,
        cache_dir: cache_dir.to_string_lossy().into_owned(),
        scale: None,
        with_geometry: false,
    };
    let client = reqwest::Client::new();
    page_render::render_pages(&pool, &client, port, &token, &book_id, &request, |event| {
        let _ = app.emit("render-pages-progress", &event);
    })
    .await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn import_book(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    manager: tauri::State<'_, Arc<ModelManager>>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    controls: tauri::State<'_, Arc<ImportControls>>,
    path: String,
    layout: Option<bool>,
) -> Result<library::ImportResult, String> {
    if controls.inner().active().is_some() {
        return Err("Another book import is already in progress".to_string());
    }
    let lite = engine_install::capability::lite_engine_active(
        &storage::paths::resolve_app_data_dir(&app)?,
    );
    let layout = if lite { Some(false) } else { layout };
    let (port, token) = supervisor
        .connection()
        .ok_or("engine is not ready".to_string())?;
    let data_dir = storage::paths::resolve_app_data_dir(&app)?;
    let books_dir = data_dir.join("books");
    let cache_dir = storage::paths::resolve_app_cache_dir(&app)?;
    let docling_dir = library::docling_artifacts_dir(manager.models_root());
    let client = reqwest::Client::new();
    let emitter = app.clone();

    let filename = std::path::Path::new(&path)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    let import_id = storage::new_id();
    let (handle, mut control) = orchestrator::stage::control_channel();
    controls.inner().register(&import_id, &filename, handle);
    let _guard = ImportScopeGuard {
        controls: Arc::clone(controls.inner()),
        import_id: import_id.clone(),
    };
    let progress_controls = Arc::clone(controls.inner());
    let progress_import_id = import_id.clone();

    let mut result = library::import_book_with_control(
        &pool,
        &client,
        port,
        &token,
        &books_dir,
        std::path::Path::new(&path),
        docling_dir,
        layout.unwrap_or(true),
        &ingest_fallback::FallbackSource::from_manager(Arc::clone(&keys)),
        Some(&mut control),
        |event| {
            if let library::ImportEvent::Progress {
                page,
                total,
                ref stage,
            } = event
            {
                progress_controls.note_progress(&progress_import_id, page, total, stage);
            }
            let _ = emitter.emit("import-progress", &event);
        },
    )
    .await?;

    let book = storage::book::get_book(&pool, &result.book.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("book '{}' not found after import", result.book.id))?;
    let request = page_render::RenderPagesRequest {
        book_path: book.source_path,
        content_hash: book.content_hash,
        pages: vec![1],
        cache_dir: cache_dir.to_string_lossy().into_owned(),
        scale: None,
        with_geometry: false,
    };
    let rendered = page_render::render_pages(
        &pool,
        &client,
        port,
        &token,
        &result.book.id,
        &request,
        |_| {},
    )
    .await;
    match rendered {
        Ok(pages) => {
            if let Some(first) = pages.first() {
                match library::attach_cover(
                    &pool,
                    &cache_dir,
                    &books_dir,
                    &result.book.id,
                    &first.hash,
                )
                .await
                {
                    Ok(cover_path) => result.book.cover_path = Some(cover_path),
                    Err(error) => tracing::warn!("cover generation failed: {error}"),
                }
            }
        }
        Err(error) => tracing::warn!("cover render failed: {error}"),
    }
    Ok(result)
}

#[tauri::command]
async fn list_books(
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
) -> Result<Vec<library::BookSummary>, String> {
    let records = storage::book::list_books(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let expected = engine_generation(&supervisor).await;
    let mut summaries = Vec::with_capacity(records.len());
    for record in &records {
        summaries.push(library::summarize(&pool, record, expected.as_deref()).await?);
    }
    Ok(summaries)
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportSummary {
    id: String,
    book_id: Option<String>,
    job_id: Option<String>,
    path: String,
    bytes: i64,
    format: String,
    mode: String,
    stale: bool,
    created_at: i64,
    duration_ms: Option<u64>,
    seek_exact: Option<bool>,
}

#[tauri::command]
async fn list_exports(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<Vec<ExportSummary>, String> {
    let rows = storage::export::list_exports(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let job_ids: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.job_id.as_deref())
        .collect();
    let evidence = storage::export::evidence_for_jobs(&pool, &job_ids)
        .await
        .map_err(|e| e.to_string())?;
    let mp3_paths: Vec<Option<std::path::PathBuf>> = rows
        .iter()
        .map(|row| {
            row.format
                .eq_ignore_ascii_case("mp3")
                .then(|| std::path::PathBuf::from(&row.path))
        })
        .collect();
    let probes = tauri::async_runtime::spawn_blocking(move || {
        mp3_paths
            .iter()
            .map(|path| path.as_deref().and_then(mp3_probe::probe_file))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .zip(probes)
        .map(|(row, probe)| {
            let current = row
                .job_id
                .as_deref()
                .and_then(|job_id| evidence.get(job_id))
                .cloned()
                .unwrap_or(storage::export::ChapterEvidence::Unattributed);
            let stale = storage::export::is_stale(row.chapter_digest.as_deref(), &current);
            ExportSummary {
                id: row.id.clone(),
                book_id: row.book_id.clone(),
                job_id: row.job_id.clone(),
                path: row.path.clone(),
                bytes: row.bytes,
                format: row.format.clone(),
                mode: row.mode.clone(),
                stale,
                created_at: row.created_at,
                duration_ms: probe.and_then(|p| p.duration_ms),
                seek_exact: probe.and_then(|p| p.seek_exact),
            }
        })
        .collect())
}

#[tauri::command]
async fn delete_export(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    export_id: String,
) -> Result<(), String> {
    let dirs = storage_dirs(&app, &pool).await?;
    delete_export_and_row(&pool, dirs.exports_dir(), &export_id).await
}

async fn delete_export_and_row(
    pool: &storage::SqlitePool,
    exports_dir: &std::path::Path,
    export_id: &str,
) -> Result<(), String> {
    let record = storage::export::get_export(pool, export_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("export '{export_id}' not found"))?;
    // `remove_within_guarded`, not a bare `remove_file`: it re-checks
    // containment, it drops the abandoned `.partial` sibling a crash can leave
    // next to the file (which would otherwise keep the directory occupied so
    // nothing can prune it), and it prunes the cast directory this file was the
    // last of. A flat removal would leave an empty directory behind that no row
    // and no sweep accounts for. The guarded variant because this command takes
    // no mutation gate and therefore runs beside a live render, whose `.partial`
    // is the file ffmpeg has open.
    if !storage::export::remove_within_guarded(pool, exports_dir, &record.path).await {
        return Err(format!("export '{export_id}' could not be removed"));
    }
    storage::export::delete_export(pool, export_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PurgeSummary {
    deleted: usize,
    failed: usize,
}

#[tauri::command]
async fn delete_all_exports(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<PurgeSummary, String> {
    let dirs = storage_dirs(&app, &pool).await?;
    let outcome = storage::export::purge_all(&pool, dirs.exports_dir())
        .await
        .map_err(|e| e.to_string())?;
    Ok(PurgeSummary {
        deleted: outcome.deleted,
        failed: outcome.failed,
    })
}

#[tauri::command]
async fn delete_book(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    book_id: String,
) -> Result<(), String> {
    storage::book::get_book(&pool, &book_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("book '{book_id}' not found"))?;
    let data_dir = storage::paths::resolve_app_data_dir(&app)?;
    let dirs = storage_dirs(&app, &pool).await?;
    let export_paths = storage::export::paths_for_book(&pool, &book_id)
        .await
        .map_err(|e| e.to_string())?;
    match storage::book::delete_book_unless_busy(&pool, &book_id, storage::now_ms())
        .await
        .map_err(|e| e.to_string())?
    {
        Ok(()) => {}
        Err(storage::book::DeleteRefusal::NoSuchBook) => {
            return Err(format!("book '{book_id}' not found"));
        }
        Err(storage::book::DeleteRefusal::ExtractionActive) => {
            return Err(
                "ERR_EXTRACTION_ACTIVE: this book's text is being re-extracted; stop that first"
                    .into(),
            );
        }
        Err(storage::book::DeleteRefusal::JobActive) => {
            return Err(
                "ERR_JOB_ACTIVE: an audio job is queued, running or paused for this book; \
                 cancel it first"
                    .into(),
            );
        }
    }
    let books_root = data_dir.join("books");
    let removed_id = book_id.clone();
    let _ = tokio::task::spawn_blocking(move || {
        storage::book_files::remove_deleted_book_files(&books_root, &removed_id)
    })
    .await;
    storage::export::purge_orphaned_paths(&pool, dirs.exports_dir(), export_paths)
        .await
        .map_err(|e| e.to_string())?;
    // Consent cascades with the book (VOICE_AGENT §5 privacy defaults): a
    // deleted book's reverse-anchor consents must not outlive it.
    storage::settings::delete_settings_with_prefix(
        &pool,
        &voice::keys::manager::reverse_anchor_consent_prefix_for_book(&book_id),
    )
    .await
    .map_err(|e| e.to_string())?;
    providers::cast_plan::delete_book_consents(&pool, &book_id).await?;
    Ok(())
}

#[tauri::command]
async fn get_chapters(
    pool: tauri::State<'_, storage::SqlitePool>,
    book_id: String,
) -> Result<Vec<ChapterSummary>, String> {
    let chapters = storage::book::list_chapters(&pool, &book_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(chapters.into_iter().map(ChapterSummary::from).collect())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ChapterSummary {
    id: String,
    idx: i64,
    title: String,
    page_start: i64,
    page_end: i64,
    is_pseudo: bool,
    word_count: Option<i64>,
}

impl From<storage::book::Chapter> for ChapterSummary {
    fn from(record: storage::book::Chapter) -> Self {
        ChapterSummary {
            id: record.id,
            idx: record.idx,
            title: record.title,
            page_start: record.page_start,
            page_end: record.page_end,
            is_pseudo: record.is_pseudo,
            word_count: record.word_count,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceProfileSummary {
    id: String,
    name: String,
    engine: String,
    kind: String,
    preset_voice_id: Option<String>,
}

#[tauri::command]
async fn list_voice_profiles(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<Vec<VoiceProfileSummary>, String> {
    let profiles = storage::voice::list_voice_profiles(&pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(profiles
        .into_iter()
        // Cloud voices are a Cast-form selection surface (A15), not local
        // voices: they never enter the local list, so no local consumer
        // (fit, language, preview, defaults) can misread them as installed
        // models (Hermes R2 cloud-profile ruling 2).
        .filter(|p| voices::is_listed_local_voice(&p.engine, p.preset_voice_id.as_deref()))
        .map(|p| VoiceProfileSummary {
            id: p.id,
            name: p.name,
            engine: p.engine,
            kind: p.kind,
            preset_voice_id: p.preset_voice_id,
        })
        .collect())
}

#[tauri::command]
async fn preview_voice(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    manager: tauri::State<'_, Arc<ModelManager>>,
    voice_profile_id: String,
    language: Option<String>,
) -> Result<String, String> {
    let profile = storage::voice::get_voice_profile(&pool, &voice_profile_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("voice profile '{voice_profile_id}' not found"))?;
    if profile.engine == providers::CLOUD_VOICE_ENGINE {
        // No paid preview (Hermes R2 cloud-profile ruling 5): previewing a
        // cloud voice would be a billable provider request outside a cast.
        return Err(
            "Cloud voices have no local preview; they synthesize only inside a cast you              start."
                .to_string(),
        );
    }
    let install = storage::models::get_model_install(&pool, &profile.engine)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("model '{}' is not installed", profile.engine))?;
    let model_dir = manager.models_root().join(&install.path);
    let voice_params: serde_json::Value = profile
        .params_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(serde_json::Value::Null);
    let (text, payload) = if profile.kind == "cloned" {
        let reference = profile
            .reference_wav_path
            .clone()
            .ok_or("this voice has no recording saved".to_string())?;
        (
            voices::sample_sentence(language.as_deref().unwrap_or("en")),
            serde_json::json!({
                "id": profile.id,
                "kind": "cloned",
                "reference_wav_path": reference,
                "params": voice_params,
            }),
        )
    } else {
        let voice_id = profile
            .preset_voice_id
            .clone()
            .ok_or("voice profile has no preset voice".to_string())?;
        let text = voices::preview_sentence_for_lang(&voice_id, language.as_deref());
        (
            text,
            serde_json::json!({
                "preset_voice_id": voice_id,
                "params": voice_params,
            }),
        )
    };
    synthesize_preview(
        &app,
        &pool,
        &supervisor,
        PreviewTarget {
            engine: &profile.engine,
            model_dir: &model_dir,
            language: language.as_deref(),
        },
        text,
        payload,
    )
    .await
}

struct PreviewTarget<'a> {
    engine: &'a str,
    model_dir: &'a std::path::Path,
    language: Option<&'a str>,
}

async fn synthesize_preview(
    app: &tauri::AppHandle,
    pool: &storage::SqlitePool,
    supervisor: &SupervisorHandle,
    target: PreviewTarget<'_>,
    text: &str,
    voice_payload: serde_json::Value,
) -> Result<String, String> {
    engine_gate(
        app,
        supervisor,
        engine_install::capability::EngineNeed::LocalAi,
    )?;
    let (port, token) = supervisor
        .connection()
        .ok_or("engine is not ready".to_string())?;
    let cache_dir = storage::paths::resolve_app_cache_dir(app)?;
    let body = serde_json::json!({
        "segments": [{"idx": 0, "text": text}],
        "voice_profile": voice_payload,
        "model_id": target.engine,
        "model_dir": target.model_dir.to_string_lossy(),
        "cache_dir": cache_dir.to_string_lossy(),
        "resource_plan": {"device": "cpu", "tts_workers": 1},
        "language": target.language,
    });
    let client = reqwest::Client::new();
    let run_id = format!("voice-preview:{}", storage::new_id());
    let audio_hash = Arc::new(Mutex::new(None::<String>));
    let sink = audio_hash.clone();
    let error = Arc::new(Mutex::new(None::<String>));
    let error_sink = error.clone();
    sidecar::client::post_sse(
        &client,
        port,
        &token,
        "/tts",
        &run_id,
        &body,
        move |event| match event.event.as_str() {
            "result" => {
                let hash = event
                    .data
                    .get("result")
                    .and_then(|r| r.get("audio_hash"))
                    .and_then(|h| h.as_str())
                    .map(|h| h.to_string());
                if let (Some(hash), Ok(mut slot)) = (hash, sink.lock()) {
                    *slot = Some(hash);
                }
            }
            "error" => {
                let message = event
                    .data
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("preview failed")
                    .to_string();
                if let Ok(mut slot) = error_sink.lock() {
                    *slot = Some(message);
                }
            }
            _ => {}
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    if let Ok(slot) = error.lock() {
        if let Some(message) = slot.as_ref() {
            return Err(message.clone());
        }
    }
    let hash = audio_hash
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .filter(|hash| hash.len() >= 2 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or("preview produced no audio".to_string())?;
    publish_preview(pool, &cache_dir, &hash).await
}

pub async fn publish_preview(
    pool: &storage::SqlitePool,
    cache_dir: &std::path::Path,
    hash: &str,
) -> Result<String, String> {
    let wav_path = render::mp3::cache_wav_path(cache_dir, hash);
    let wav_bytes = std::fs::metadata(&wav_path)
        .map(|m| m.len() as i64)
        .map_err(|_| "preview audio missing from cache".to_string())?;
    storage::cache::record_use(pool, hash, "wav", wav_bytes, None)
        .await
        .map_err(|e| e.to_string())?;
    let preview_dir = cache_dir.join(cache_gc::PREVIEW_DIR);
    std::fs::create_dir_all(&preview_dir).map_err(|e| e.to_string())?;
    let preview_path = preview_dir.join(format!("{hash}.wav"));
    if !preview_path.is_file() {
        std::fs::copy(&wav_path, &preview_path).map_err(|e| e.to_string())?;
    }
    Ok(preview_path.to_string_lossy().into_owned())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceSample {
    sample_id: String,
    seconds: f64,
    hit_hard_stop: bool,
}

fn voices_root(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    storage::paths::resolve_app_data_dir(app)
}

fn checked_sample_path(
    app: &tauri::AppHandle,
    sample_id: &str,
) -> Result<std::path::PathBuf, String> {
    if !voice_sample::is_sample_id(sample_id) {
        return Err("that recording is no longer available".to_string());
    }
    Ok(voice_sample::staging_path(&voices_root(app)?, sample_id))
}

fn cloning_engine(pool_installs: &[storage::models::ModelInstall]) -> Option<String> {
    let catalog = catalog::bundled_catalog();
    catalog
        .models
        .iter()
        .find(|entry| {
            entry.task == catalog::ModelTask::Tts
                && entry.capabilities.iter().any(|c| c == "voice_cloning")
                && pool_installs.iter().any(|i| i.model_id == entry.id)
        })
        .map(|entry| entry.id.clone())
}

#[tauri::command]
async fn list_audio_inputs() -> Result<Vec<voice_capture::AudioInput>, String> {
    tokio::task::spawn_blocking(voice_capture::list_inputs)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_voice_recording(
    app: tauri::AppHandle,
    recorder: tauri::State<'_, Arc<voice_capture::Recorder>>,
    device_name: Option<String>,
) -> Result<String, String> {
    let sample_id = storage::new_id();
    let raw = voice_sample::staging_path(&voices_root(&app)?, &format!("{sample_id}-raw"));
    let recorder = recorder.inner().clone();
    let staged_id = sample_id.clone();
    tokio::task::spawn_blocking(move || recorder.start(&raw, &staged_id, device_name.as_deref()))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(sample_id)
}

#[tauri::command]
async fn stop_voice_recording(
    app: tauri::AppHandle,
    recorder: tauri::State<'_, Arc<voice_capture::Recorder>>,
) -> Result<VoiceSample, String> {
    let stopped = recorder.stop().map_err(|e| e.to_string())?;
    let target = checked_sample_path(&app, &stopped.sample_id)?;
    let seconds = voice_sample::condition_into(&stopped.path, &target)
        .await
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&stopped.path);
    Ok(VoiceSample {
        sample_id: stopped.sample_id,
        seconds: seconds?,
        hit_hard_stop: stopped.hit_hard_stop,
    })
}

#[tauri::command]
fn cancel_voice_recording(recorder: tauri::State<'_, Arc<voice_capture::Recorder>>) {
    recorder.cancel();
}

#[tauri::command]
async fn import_voice_sample(app: tauri::AppHandle, path: String) -> Result<VoiceSample, String> {
    let source = std::path::PathBuf::from(&path);
    if !source.is_file() {
        return Err(format!("'{path}' is not a file txtCaster can open."));
    }
    let sample_id = storage::new_id();
    let target = checked_sample_path(&app, &sample_id)?;
    let seconds = voice_sample::condition_into(&source, &target)
        .await
        .map_err(|e| e.to_string())?;
    Ok(VoiceSample {
        sample_id,
        seconds,
        hit_hard_stop: false,
    })
}

#[tauri::command]
fn discard_voice_sample(app: tauri::AppHandle, sample_id: String) -> Result<(), String> {
    let path = checked_sample_path(&app, &sample_id)?;
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[tauri::command]
async fn preview_voice_sample(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    manager: tauri::State<'_, Arc<ModelManager>>,
    sample_id: String,
    engine: Option<String>,
    language: Option<String>,
) -> Result<String, String> {
    let reference = checked_sample_path(&app, &sample_id)?;
    if !reference.is_file() {
        return Err("that recording is no longer available".to_string());
    }
    let installs = storage::models::list_model_installs(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let resolved_engine = if let Some(eng) = engine {
        if installs.iter().any(|i| i.model_id == eng) {
            eng
        } else {
            return Err(format!("model '{eng}' is not installed"));
        }
    } else {
        cloning_engine(&installs).ok_or_else(|| {
            "Voice cloning needs a cloning-capable model. Install it from Models, then try again."
                .to_string()
        })?
    };
    let install = installs
        .iter()
        .find(|i| i.model_id == resolved_engine)
        .ok_or_else(|| format!("model '{resolved_engine}' is not installed"))?;
    let model_dir = manager.models_root().join(&install.path);
    let payload = serde_json::json!({
        "id": format!("sample-{sample_id}"),
        "kind": "cloned",
        "reference_wav_path": reference.to_string_lossy(),
        "params": {},
    });
    let sample_lang = language.as_deref().unwrap_or("en");
    synthesize_preview(
        &app,
        &pool,
        &supervisor,
        PreviewTarget {
            engine: &resolved_engine,
            model_dir: &model_dir,
            language: Some(sample_lang),
        },
        voices::sample_sentence(sample_lang),
        payload,
    )
    .await
}

#[tauri::command]
async fn save_voice_profile(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    sample_id: String,
    name: String,
    engine: Option<String>,
) -> Result<VoiceProfileSummary, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Give this voice a name so you can find it later.".to_string());
    }
    let staged = checked_sample_path(&app, &sample_id)?;
    if !staged.is_file() {
        return Err("that recording is no longer available".to_string());
    }
    let installs = storage::models::list_model_installs(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let resolved_engine = if let Some(eng) = engine {
        if installs.iter().any(|i| i.model_id == eng) {
            eng
        } else {
            return Err(format!("model '{eng}' is not installed"));
        }
    } else {
        cloning_engine(&installs).ok_or_else(|| {
            "Voice cloning needs a cloning-capable model. Install it from Models, then try again."
                .to_string()
        })?
    };

    let data_dir = voices_root(&app)?;
    let profile_id = storage::new_id();
    let target = voice_sample::reference_path(&data_dir, &profile_id);
    std::fs::create_dir_all(target.parent().expect("reference has a parent"))
        .map_err(|e| e.to_string())?;
    std::fs::rename(&staged, &target)
        .map_err(|e| format!("the recording could not be saved: {e}"))?;

    let digest = {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(&target).map_err(|e| e.to_string())?;
        hex::encode(Sha256::digest(&bytes))
    };
    let params = serde_json::json!({ "ref_sha256": digest }).to_string();
    let profile = storage::voice::VoiceProfile {
        id: profile_id,
        name: trimmed.to_string(),
        engine: resolved_engine,
        kind: "cloned".to_string(),
        preset_voice_id: None,
        reference_wav_path: Some(target.to_string_lossy().into_owned()),
        params_json: Some(params),
        created_at: storage::now_ms(),
    };
    if let Err(err) = storage::voice::insert_voice_profile(&pool, &profile).await {
        let _ = std::fs::remove_dir_all(target.parent().expect("reference has a parent"));
        return Err(err.to_string());
    }
    Ok(VoiceProfileSummary {
        id: profile.id,
        name: profile.name,
        engine: profile.engine,
        kind: profile.kind,
        preset_voice_id: profile.preset_voice_id,
    })
}

#[tauri::command]
async fn delete_voice_profile(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    voice_profile_id: String,
) -> Result<(), String> {
    if !voice_sample::is_profile_id(&voice_profile_id) {
        return Err(format!("voice profile '{voice_profile_id}' not found"));
    }
    let profile = storage::voice::get_voice_profile(&pool, &voice_profile_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("voice profile '{voice_profile_id}' not found"))?;
    if profile.kind != "cloned" && profile.engine != providers::CLOUD_VOICE_ENGINE {
        return Err("Built-in voices cannot be deleted.".to_string());
    }
    let used = storage::job::count_jobs_for_voice_profile(&pool, &voice_profile_id)
        .await
        .map_err(|e| e.to_string())?;
    if used > 0 {
        let plural = if used == 1 { "job" } else { "jobs" };
        return Err(format!(
            "'{}' is used by {used} {plural}. Delete those {plural} first, or keep this voice \
             so their audio can be rebuilt.",
            profile.name
        ));
    }
    storage::voice::delete_voice_profile(&pool, &voice_profile_id)
        .await
        .map_err(|e| e.to_string())?;
    let dir = voice_sample::profile_dir(&voices_root(&app)?, &voice_profile_id);
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LanSendStartResult {
    qr_svg: String,
    qr_payload: String,
    fingerprint: String,
    hosts: Vec<String>,
    port: u16,
    /// How long the QR just minted stays scannable; the modal re-mints on expiry.
    secret_ttl_ms: u64,
}

async fn probe_audio_duration_seconds(path: &std::path::Path) -> Option<f64> {
    let mut command = render::ffmpeg::probe_command();
    command.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=nw=1:nk=1",
    ]);
    command.arg(path);
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let output = command.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|&sec| sec > 0.0)
}

#[tauri::command]
async fn lan_send_start(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    sender: tauri::State<'_, lan_sender::session::LanSenderHandle>,
    export_ids: Vec<String>,
) -> Result<LanSendStartResult, String> {
    let requested: std::collections::HashSet<String> = export_ids.iter().cloned().collect();
    {
        let mut active = sender.active();
        if let Some(current) = active.as_ref() {
            if current.export_ids == requested {
                let secret = current.session.mint_secret();
                let payload = lan_sender::session::qr_payload(
                    &current.hosts,
                    current.server.fingerprint(),
                    &secret,
                );
                let qr_svg = lan_sender::session::qr_svg(&payload)?;
                return Ok(LanSendStartResult {
                    qr_svg,
                    qr_payload: payload,
                    fingerprint: current.server.fingerprint().to_string(),
                    hosts: current.hosts.clone(),
                    port: current.server.port(),
                    secret_ttl_ms: lan_sender::session::PAIR_SECRET_TTL.as_millis() as u64,
                });
            }
        }
        if let Some(previous) = active.take() {
            previous.server.stop();
        }
    }
    let lan_ips = lan_sender::server::lan_ips();
    if !lan_ips.iter().any(lan_sender::session::advertisable) {
        return Err("no lan address".into());
    }
    let dirs = storage_dirs(&app, &pool).await?;
    let exports_dir = dirs.exports_dir();
    let mut sources = Vec::with_capacity(export_ids.len());
    for id in &export_ids {
        let row = storage::export::get_export(&pool, id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("export '{id}' not found"))?;
        let path = match storage::export::resolve_within(exports_dir, &row.path)? {
            Some(resolved) => resolved,
            None => std::path::PathBuf::from(&row.path),
        };
        if storage::export::is_path_stale(&pool, &row.path)
            .await
            .map_err(|e| e.to_string())?
        {
            return Err(format!(
                "export '{id}' is stale and needs to be re-rendered"
            ));
        }
        let title = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Audiobook")
            .to_string();

        let cover_path = if let Some(ref book_id) = row.book_id {
            match storage::book::get_book(&pool, book_id).await {
                Ok(Some(b)) => b
                    .cover_path
                    .map(std::path::PathBuf::from)
                    .filter(|p| p.is_file()),
                _ => None,
            }
        } else {
            None
        };

        let alignment_path = if let Some(ref job_id) = row.job_id {
            let staging_dir = exports_dir.join(".staging").join(&row.id);
            if let Ok(()) = tokio::fs::create_dir_all(&staging_dir).await {
                let align_file = staging_dir.join("alignment.json");
                let cache_dir = storage::paths::resolve_app_cache_dir(&app)
                    .unwrap_or_else(|_| exports_dir.join(".cache"));
                match reading::build_export_alignment(&pool, job_id, &cache_dir).await {
                    Ok(val) => {
                        if let Ok(bytes) = serde_json::to_vec_pretty(&val) {
                            if tokio::fs::write(&align_file, bytes).await.is_ok() {
                                Some(align_file)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    Err(e) => {
                        tracing::warn!(job_id, error = %e, "could not build export alignment");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let duration_seconds = probe_audio_duration_seconds(&path).await;

        sources.push(lan_sender::session::CastSource {
            cast_id: row.id,
            title,
            path,
            cover_path,
            alignment_path,
            duration_seconds,
        });
    }
    let emit_app = app.clone();
    let session = Arc::new(lan_sender::session::SessionState::new(
        sources,
        lan_sender::proto::DEFAULT_CHUNK_SIZE as u64,
        Arc::new(move |snapshot| {
            let _ = emit_app.emit("lan-send-progress", &snapshot);
        }),
    )?);
    let server = lan_sender::server::start(session.clone()).await?;
    let hosts = lan_sender::session::advertised_hosts(&lan_ips, server.port());
    let secret = session.mint_secret();
    let payload = lan_sender::session::qr_payload(&hosts, server.fingerprint(), &secret);
    let qr_svg = lan_sender::session::qr_svg(&payload)?;
    let result = LanSendStartResult {
        qr_svg,
        qr_payload: payload,
        fingerprint: server.fingerprint().to_string(),
        hosts: hosts.clone(),
        port: server.port(),
        secret_ttl_ms: lan_sender::session::PAIR_SECRET_TTL.as_millis() as u64,
    };
    *sender.active() = Some(lan_sender::session::ActiveSend {
        session,
        server,
        export_ids: requested,
        hosts,
    });
    Ok(result)
}

#[tauri::command]
fn lan_send_status(
    sender: tauri::State<'_, lan_sender::session::LanSenderHandle>,
) -> Option<lan_sender::session::SessionSnapshot> {
    sender
        .active()
        .as_ref()
        .map(|active| active.session.snapshot())
}

#[tauri::command]
fn lan_send_stop(sender: tauri::State<'_, lan_sender::session::LanSenderHandle>) {
    if let Some(active) = sender.active().take() {
        active.server.stop();
    }
}

struct MediaRoots(Vec<std::path::PathBuf>);

/// The only directories the local media server will hand to the webview. Voice recordings are
/// deliberately absent: nothing plays them back directly — a preview is synthesised into the
/// cache first — and `import_voice_sample` takes a caller-supplied path, so a servable voices
/// directory would turn it into a way to republish any audio file the user can read.
fn media_roots(
    dirs: &storage::paths::StorageDirs,
    data_dir: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    vec![dirs.exports_dir().to_path_buf(), data_dir.join("cache")]
}

fn media_roots_with_cache(
    dirs: &storage::paths::StorageDirs,
    data_dir: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let mut roots = media_roots(dirs, data_dir);
    if !roots.contains(&dirs.cache_dir) {
        roots.push(dirs.cache_dir.clone());
    }
    roots
}

#[tauri::command]
fn log_ui_layout(line: String) {
    tracing::debug!(target: "ui", "ui layout: {}", diagnostics::diagnostic_line(&line));
}

#[tauri::command]
fn log_reading_clock(line: String) {
    tracing::debug!(target: "ui", "reading clock: {}", diagnostics::diagnostic_line(&line));
}

#[tauri::command]
fn log_page_mode(line: String) {
    tracing::debug!(
        target: "ui",
        "{}: {}",
        reading::PAGE_MODE_LOG_PREFIX,
        diagnostics::diagnostic_line(&line)
    );
}

#[tauri::command]
fn log_follow_state(line: String) {
    tracing::debug!(
        target: "ui",
        "{}: {}",
        reading::FOLLOW_STATE_LOG_PREFIX,
        diagnostics::diagnostic_line(&line)
    );
}

#[tauri::command]
fn log_reading_tap(line: String) {
    tracing::debug!(
        target: "ui",
        "{} {}",
        reading::READING_TAP_LOG_PREFIX,
        diagnostics::diagnostic_line(&line)
    );
}

#[tauri::command]
fn log_ui_error(line: String) {
    tracing::error!(target: "ui", "ui crashed: {}", diagnostics::ui_crash_digest(&line));
}

#[tauri::command]
async fn get_media_url(
    pool: tauri::State<'_, storage::SqlitePool>,
    server: tauri::State<'_, media_server::MediaServerHandle>,
    roots: tauri::State<'_, MediaRoots>,
    path: String,
) -> Result<String, String> {
    media_url_for(&pool, &server, &roots.0, &path).await
}

pub const STALE_EXPORT_MESSAGE: &str =
    "this audio no longer matches the book's chapters; re-render it before playing";

pub async fn media_url_for(
    pool: &storage::SqlitePool,
    server: &media_server::MediaServerHandle,
    roots: &[std::path::PathBuf],
    path: &str,
) -> Result<String, String> {
    if storage::export::is_path_stale(pool, path)
        .await
        .map_err(|e| e.to_string())?
    {
        server.forget(std::path::Path::new(path));
        return Err(STALE_EXPORT_MESSAGE.to_string());
    }
    server
        .register(std::path::Path::new(path), roots)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct JobSummary {
    id: String,
    book_id: String,
    scope: Scope,
    mode: String,
    voice_profile_id: String,
    output_format: String,
    state: String,
    checkpoint_json: Option<String>,
    overnight: bool,
    eta_seconds: Option<i64>,
    report_json: Option<String>,
    output_paths_json: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl TryFrom<storage::job::RenderJob> for JobSummary {
    type Error = String;

    fn try_from(record: storage::job::RenderJob) -> Result<Self, Self::Error> {
        let scope: Scope = serde_json::from_str(&record.scope_json).map_err(|e| e.to_string())?;
        Ok(JobSummary {
            id: record.id,
            book_id: record.book_id,
            scope,
            mode: record.mode,
            voice_profile_id: record.voice_profile_id,
            output_format: record.output_format,
            state: record.state,
            checkpoint_json: record.checkpoint_json,
            overnight: record.overnight,
            eta_seconds: record.eta_seconds,
            report_json: record.report_json,
            output_paths_json: record.output_paths_json,
            created_at: record.created_at,
            updated_at: record.updated_at,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SegmentSummary {
    id: String,
    chapter_id: String,
    idx: i64,
    page_no: i64,
    kind: String,
    text: String,
    flags: Vec<String>,
    has_audio: bool,
    duration_ms: Option<i64>,
    edited: bool,
    wer: Option<f64>,
    align_source: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateJobArgs {
    book_id: String,
    scope: Scope,
    mode: String,
    output_format: String,
    voice_profile_id: Option<String>,
    model_overrides_json: Option<String>,
    #[serde(default)]
    overnight: bool,
    #[serde(default)]
    verify: bool,
    /// S-4 cloud phase selections (pre-brief item 1): resolved into recorded
    /// bindings + consent proof + cost snapshot at this boundary, atomically
    /// with the job row. Absent = the untouched local path (T7).
    #[serde(default)]
    script_cloud: Option<providers::cast_plan::CloudScriptSelection>,
    #[serde(default)]
    tts_cloud: Option<providers::cast_plan::CloudTtsSelection>,
}

fn engine_gate(
    app: &tauri::AppHandle,
    supervisor: &SupervisorHandle,
    need: engine_install::capability::EngineNeed,
) -> Result<(), String> {
    let data_dir = storage::paths::resolve_app_data_dir(app)?;
    match engine_install::capability::refusal(
        &supervisor.status(),
        engine_install::capability::lite_engine_active(&data_dir),
        need,
    ) {
        Some(code) => Err(engine_install::capability::refusal_message(code, need)),
        None => Ok(()),
    }
}

#[tauri::command]
async fn create_job(
    app: tauri::AppHandle,
    orchestrator: tauri::State<'_, Arc<JobOrchestrator>>,
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    args: CreateJobArgs,
) -> Result<String, String> {
    // Hermes R2 blocker 1: writer-owned keys are rejected UNCONDITIONALLY,
    // on every path — a forged cost/consent snapshot on a local-only job is
    // still a forged audit fact.
    providers::cast_plan::reject_writer_owned_keys(args.model_overrides_json.as_deref())?;
    let need = engine_install::capability::cast_need(
        args.script_cloud.is_some(),
        args.tts_cloud.is_some(),
        args.mode == "teacher",
        orchestrator::scripting::narrator_llm_enabled(&pool)
            .await
            .map_err(|e| e.to_string())?,
    );
    engine_gate(&app, &supervisor, need)?;
    if args.script_cloud.is_none() && args.tts_cloud.is_none() {
        let request = CreateJobRequest {
            book_id: args.book_id,
            scope: args.scope,
            mode: args.mode,
            voice_profile_id: args
                .voice_profile_id
                .unwrap_or_else(|| orchestrator::DEFAULT_VOICE_PROFILE_ID.to_string()),
            output_format: args.output_format,
            model_overrides_json: args.model_overrides_json,
            overnight: args.overnight,
            verify: args.verify,
        };
        return orchestrator
            .create_job(request)
            .await
            .map_err(|e| e.to_string());
    }
    // Race-safe writer (Hermes R2 fold-in 2): resolution, consent
    // validation, snapshot construction, and job insertion run under one
    // process-wide writer lock, with the plan — including its consent
    // re-check — computed INSIDE the guarded section, so a grant revoked or
    // a connection edited concurrently is seen before the job row exists.
    let _writer = providers::cast_plan::cast_writer_mutex().lock().await;
    let plan = providers::cast_plan::plan_cast(
        &pool,
        &args.book_id,
        args.model_overrides_json.as_deref(),
        args.script_cloud.as_ref(),
        args.tts_cloud.as_ref(),
        args.scope.clone(),
        &args.mode,
        storage::now_ms() / 1000,
    )
    .await?;
    if !plan.consent_missing.is_empty() {
        // Structured refusal: the form opens the consent sheet and
        // re-submits after a grant. No cloud request has started.
        let payload =
            serde_json::to_string(&plan.consent_missing).unwrap_or_else(|_| "[]".to_string());
        return Err(format!("CONSENT_REQUIRED:{payload}"));
    }
    let request = CreateJobRequest {
        book_id: args.book_id,
        scope: args.scope,
        mode: args.mode,
        // Truthful attribution: cloud TTS records the SELECTED cloud
        // profile as the job's voice.
        voice_profile_id: plan
            .voice_profile_id
            .clone()
            .or(args.voice_profile_id)
            .unwrap_or_else(|| orchestrator::DEFAULT_VOICE_PROFILE_ID.to_string()),
        output_format: args.output_format,
        model_overrides_json: Some(plan.overrides_json),
        overnight: args.overnight,
        verify: args.verify,
    };
    orchestrator
        .create_job(request)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pause_job(
    orchestrator: tauri::State<'_, Arc<JobOrchestrator>>,
    job_id: String,
) -> Result<(), String> {
    orchestrator
        .pause_job(&job_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn resume_job(
    orchestrator: tauri::State<'_, Arc<JobOrchestrator>>,
    job_id: String,
) -> Result<(), String> {
    orchestrator
        .resume_job(&job_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn switch_job_to_overnight(
    orchestrator: tauri::State<'_, Arc<JobOrchestrator>>,
    job_id: String,
) -> Result<(), String> {
    orchestrator
        .switch_to_overnight(&job_id)
        .await
        .map_err(|e| e.to_string())
}

/// Explicit provider-failure recovery (pre-brief item 9): retry the same
/// recorded binding, or — a user decision, never automatic — rewrite the
/// plan to Local with a recorded recovery fact, then re-queue. Bindings
/// and checkpoints are otherwise preserved.
#[tauri::command]
async fn retry_failed_job(
    app: tauri::AppHandle,
    orchestrator: tauri::State<'_, Arc<JobOrchestrator>>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    job_id: String,
    switch_to_local: bool,
) -> Result<(), String> {
    if switch_to_local {
        engine_gate(
            &app,
            &supervisor,
            engine_install::capability::EngineNeed::LocalAi,
        )?;
    }
    orchestrator
        .recover_failed(&job_id, switch_to_local)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cancel_job(
    orchestrator: tauri::State<'_, Arc<JobOrchestrator>>,
    job_id: String,
) -> Result<(), String> {
    orchestrator
        .cancel_job(&job_id)
        .await
        .map_err(|e| e.to_string())
}

/// F11-U6 J7.4 (spec §6.3 "a live session's row blocks the cast delete"):
/// the settled-state rule as before, preceded by the live guard. `live_job`
/// is the cast the slot's live tutor session runs on; deleting that cast
/// would cascade the journal row the actor is still writing (R2 keeps the
/// cascade, so the refusal is the only protection). Held under the slot lock
/// by the caller so no session can start on the row in between (DQA-H5).
pub(crate) async fn delete_job_guarded(
    pool: &storage::SqlitePool,
    job_id: &str,
    live_job: Option<&str>,
) -> Result<(), String> {
    if live_job == Some(job_id) {
        return Err("cannot delete a cast with a live tutor session; end the session first".into());
    }
    let deleted = storage::job::delete_job_if_settled(pool, job_id)
        .await
        .map_err(|e| e.to_string())?;
    if !deleted {
        let record = storage::job::get_job(pool, job_id)
            .await
            .map_err(|e| e.to_string())?;
        return Err(match record {
            Some(record) => format!(
                "cannot delete job in state {}; pause or cancel it first",
                record.state
            ),
            None => format!("job '{job_id}' not found"),
        });
    }
    Ok(())
}

#[tauri::command]
async fn delete_job(
    pool: tauri::State<'_, storage::SqlitePool>,
    slot: tauri::State<'_, VoiceSessionSlot>,
    job_id: String,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    let live_job = session
        .as_ref()
        .filter(|handle| handle.is_alive())
        .and_then(|handle| handle.job_id());
    delete_job_guarded(&pool, &job_id, live_job).await
}

/// F11-U6 §6.1 (D-8 ruled (c)): read the transcript-retention choice as the
/// two ruled values. An absent key is the default and is written as `keep`
/// once so Settings shows a stored choice; an unknown value reads as `keep`
/// (the journal treats anything but `never` as persisted) and is left alone.
pub(crate) async fn retention_read(pool: &storage::SqlitePool) -> Result<String, String> {
    let stored = storage::settings::get_setting(pool, voice_journal::TRANSCRIPT_RETENTION_KEY)
        .await
        .map_err(|e| e.to_string())?;
    match stored.as_deref() {
        Some(voice_journal::RETENTION_NEVER) => Ok(voice_journal::RETENTION_NEVER.to_string()),
        Some(_) => Ok(voice_journal::RETENTION_KEEP.to_string()),
        None => {
            retention_write(pool, voice_journal::RETENTION_KEEP).await?;
            Ok(voice_journal::RETENTION_KEEP.to_string())
        }
    }
}

/// Writes one of the two ruled values; anything else is refused unchanged.
/// Applies from the next session (`JournalMode::from_settings` decides once).
pub(crate) async fn retention_write(
    pool: &storage::SqlitePool,
    value: &str,
) -> Result<String, String> {
    if value != voice_journal::RETENTION_KEEP && value != voice_journal::RETENTION_NEVER {
        return Err("retention must be \"keep\" or \"never\"".into());
    }
    storage::settings::set_setting(pool, voice_journal::TRANSCRIPT_RETENTION_KEY, value)
        .await
        .map_err(|e| e.to_string())?;
    Ok(value.to_string())
}

#[tauri::command]
async fn voice_retention_get(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<String, String> {
    retention_read(&pool).await
}

#[tauri::command]
async fn voice_retention_set(
    pool: tauri::State<'_, storage::SqlitePool>,
    value: String,
) -> Result<String, String> {
    retention_write(&pool, &value).await
}

fn reported_worst_ids(report_json: Option<&str>) -> Vec<String> {
    let Some(raw) = report_json else {
        return Vec::new();
    };
    let Ok(report) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    report["verify"]["worst"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["segment_id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tauri::command]
async fn list_reviewable_segments(
    pool: tauri::State<'_, storage::SqlitePool>,
    job_id: String,
) -> Result<Vec<SegmentSummary>, String> {
    let record = storage::job::get_job(&pool, &job_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("job '{job_id}' not found"))?;
    let worst = reported_worst_ids(record.report_json.as_deref());
    let also: Vec<&str> = worst.iter().map(String::as_str).collect();
    let segments = storage::job::list_reviewable_segments(&pool, &job_id, &also)
        .await
        .map_err(|e| e.to_string())?;
    Ok(segments
        .into_iter()
        .map(|segment| SegmentSummary {
            id: segment.id,
            chapter_id: segment.chapter_id,
            idx: segment.idx,
            page_no: segment.page_no,
            kind: segment.kind,
            text: segment.text,
            flags: segment
                .flags_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or_default(),
            has_audio: segment.audio_hash.is_some(),
            duration_ms: segment.duration_ms,
            edited: segment.original_text.is_some(),
            wer: segment.wer,
            align_source: segment.align_source,
        })
        .collect())
}

#[tauri::command]
async fn get_reading_index(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    job_id: String,
) -> Result<reading::ReadingIndex, String> {
    let cache_dir = storage::paths::resolve_app_cache_dir(&app)?;
    reading::index(&pool, &job_id, &cache_dir)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_reading_chapter(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    job_id: String,
    chapter_id: String,
) -> Result<reading::ReadingChapter, String> {
    let cache_dir = storage::paths::resolve_app_cache_dir(&app)?;
    let pool_ref = &*pool;
    let supervisor_ref = &*supervisor;
    let cache_ref = cache_dir.as_path();
    reading::chapter(
        pool_ref,
        &job_id,
        &chapter_id,
        cache_ref,
        |book_id, pages| async move {
            let owned = book_id.clone();
            let report =
                reading::ensure_geometry_for_chapter(pool_ref, &book_id, &pages, move |batch| {
                    let owned = owned.clone();
                    async move {
                        ensure_pages_inner(pool_ref, supervisor_ref, cache_ref, &owned, &batch)
                            .await
                            .map(|_| ())
                    }
                })
                .await;
            tracing::debug!("{}", report.log_line(&book_id));
            report.outcome()
        },
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_reading_pages(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    book_id: String,
    pages: Vec<i64>,
) -> Result<Vec<reading::ReadingPage>, String> {
    if let Some(refusal) = reading::refuse_oversized(pages.len()) {
        return Err(refusal);
    }
    let cache_dir = storage::paths::resolve_app_cache_dir(&app)?;
    reading::pages(&pool, &book_id, &pages, &cache_dir)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn ensure_reading_pages(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
    book_id: String,
    pages: Vec<i64>,
) -> Result<Vec<reading::ReadingPage>, String> {
    if let Some(refusal) = reading::refuse_oversized(pages.len()) {
        return Err(refusal);
    }
    let cache_dir = storage::paths::resolve_app_cache_dir(&app)?;
    ensure_pages_inner(&pool, &supervisor, &cache_dir, &book_id, &pages).await
}

async fn ensure_pages_inner(
    pool: &storage::SqlitePool,
    supervisor: &SupervisorHandle,
    cache_dir: &std::path::Path,
    book_id: &str,
    pages: &[i64],
) -> Result<Vec<reading::ReadingPage>, String> {
    let book_id = book_id.to_string();
    let pages = pages.to_vec();
    let started = std::time::Instant::now();
    let found = reading::pages(pool, &book_id, &pages, cache_dir)
        .await
        .map_err(|e| e.to_string())?;
    let answered: std::collections::HashSet<i64> = found.iter().map(|page| page.page_no).collect();
    for page_no in &pages {
        if !answered.contains(page_no) {
            tracing::debug!(
                "{}",
                reading::format_ensure_line(
                    &book_id,
                    *page_no,
                    reading::EnsureOutcome::Absent,
                    None,
                    started.elapsed().as_millis(),
                    None,
                )
            );
        }
    }
    let slots = reading::geometry_slots(pool, &book_id, &pages)
        .await
        .map_err(|e| e.to_string())?;
    let Some((port, token)) = supervisor.connection() else {
        for page in &found {
            tracing::debug!(
                "{}",
                reading::format_ensure_line(
                    &book_id,
                    page.page_no,
                    if page.image_path.is_some() {
                        reading::EnsureOutcome::Hit
                    } else {
                        reading::EnsureOutcome::EngineDown
                    },
                    None,
                    started.elapsed().as_millis(),
                    None,
                )
            );
        }
        log_geometry(&book_id, &slots, None);
        return Ok(found);
    };
    let client = reqwest::Client::new();
    let expected = page_render::engine_geometry_version(&client, port, &token).await;
    let missing = reading::pages_to_ensure(&found, &slots, expected.as_deref());
    if missing.is_empty() {
        for page in &found {
            tracing::debug!(
                "{}",
                reading::format_ensure_line(
                    &book_id,
                    page.page_no,
                    reading::EnsureOutcome::Hit,
                    None,
                    started.elapsed().as_millis(),
                    None,
                )
            );
        }
        log_geometry(&book_id, &slots, expected.as_deref());
        return Ok(found);
    }
    let to_rasterise = found
        .iter()
        .filter(|page| page.image_path.is_none() && missing.contains(&page.page_no))
        .count();
    tracing::debug!(
        "{}",
        reading::format_ensure_plan(&book_id, missing.len(), to_rasterise)
    );
    let book = storage::book::get_book(pool, &book_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("book '{book_id}' not found"))?;
    let book_generation = book.blocks_version.clone();
    let request = page_render::RenderPagesRequest {
        book_path: book.source_path,
        content_hash: book.content_hash,
        pages: missing.clone(),
        cache_dir: cache_dir.to_string_lossy().into_owned(),
        scale: None,
        with_geometry: true,
    };
    let fence = page_render::GenerationFence {
        blocks_version: book_generation,
    };
    let render_started = std::time::Instant::now();
    let rendered = page_render::render_pages_fenced(
        pool,
        &client,
        port,
        &token,
        page_render::PersistTarget::fenced(&book_id, &fence),
        &request,
        |_| {},
    )
    .await;
    let render_ms = render_started.elapsed().as_millis();
    match rendered {
        Ok(reports) => {
            for report in &reports {
                tracing::debug!(
                    "{}",
                    reading::format_ensure_line(
                        &book_id,
                        report.page_no,
                        reading::EnsureOutcome::Rendered,
                        Some(render_ms),
                        started.elapsed().as_millis(),
                        Some(report.bytes),
                    )
                );
            }
        }
        Err(error) => {
            tracing::warn!("reading page render failed for '{book_id}': {error}");
            for page_no in &missing {
                tracing::debug!(
                    "{}",
                    reading::format_ensure_line(
                        &book_id,
                        *page_no,
                        reading::EnsureOutcome::RenderFailed,
                        Some(render_ms),
                        started.elapsed().as_millis(),
                        None,
                    )
                );
            }
            let after = reading::geometry_slots(pool, &book_id, &pages)
                .await
                .map_err(|e| e.to_string())?;
            log_geometry(&book_id, &after, expected.as_deref());
            return Ok(found);
        }
    }
    let after = reading::geometry_slots(pool, &book_id, &pages)
        .await
        .map_err(|e| e.to_string())?;
    log_geometry(&book_id, &after, expected.as_deref());
    reading::pages(pool, &book_id, &pages, cache_dir)
        .await
        .map_err(|e| e.to_string())
}

fn log_geometry(book_id: &str, slots: &[reading::GeometrySlot], expected: Option<&str>) {
    for slot in slots {
        tracing::debug!(
            "{}",
            reading::format_geometry_line(
                book_id,
                slot.page_no,
                slot.words,
                slot.lines,
                slot.bytes,
                slot.version.as_deref(),
                reading::classify_geometry(slot.version.as_deref(), expected),
            )
        );
    }
}

#[tauri::command]
async fn get_reading_figures(
    pool: tauri::State<'_, storage::SqlitePool>,
    book_id: String,
    figure_ids: Vec<String>,
) -> Result<Vec<reading::ReadingFigure>, String> {
    if let Some(refusal) = reading::refuse_oversized(figure_ids.len()) {
        return Err(refusal);
    }
    reading::figures(&pool, &book_id, &figure_ids)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_suppressed_blocks(
    pool: tauri::State<'_, storage::SqlitePool>,
    job_id: String,
) -> Result<Vec<storage::job::SuppressedBlock>, String> {
    storage::job::list_suppressed_blocks(&pool, &job_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn resynthesize_segment(
    orchestrator: tauri::State<'_, Arc<JobOrchestrator>>,
    job_id: String,
    segment_id: String,
    text: Option<String>,
) -> Result<(), String> {
    let edit = match text {
        Some(text) => SegmentEdit::Replace(text),
        None => SegmentEdit::Revert,
    };
    orchestrator
        .resynthesize_segment(&job_id, &segment_id, edit)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn rerender_job(
    orchestrator: tauri::State<'_, Arc<JobOrchestrator>>,
    job_id: String,
) -> Result<(), String> {
    orchestrator
        .rerender_job(&job_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_job(
    pool: tauri::State<'_, storage::SqlitePool>,
    job_id: String,
) -> Result<JobSummary, String> {
    let record = storage::job::get_job(&pool, &job_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("job '{job_id}' not found"))?;
    JobSummary::try_from(record)
}

#[tauri::command]
async fn list_jobs(pool: tauri::State<'_, storage::SqlitePool>) -> Result<Vec<JobSummary>, String> {
    let states: Vec<&str> = orchestrator::ALL_STATES
        .iter()
        .map(|s| s.as_str())
        .collect();
    let records = storage::job::list_jobs_in_states(&pool, &states)
        .await
        .map_err(|e| e.to_string())?;
    Ok(readable_job_summaries(records))
}

fn readable_job_summaries(records: Vec<storage::job::RenderJob>) -> Vec<JobSummary> {
    records
        .into_iter()
        .filter_map(|record| {
            let id = record.id.clone();
            JobSummary::try_from(record)
                .map_err(
                    |err| tracing::warn!(job = %id, "job list skipped an unreadable row: {err}"),
                )
                .ok()
        })
        .collect()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledModelView {
    #[serde(flatten)]
    pub install: ModelInstall,
    pub missing_files: Vec<String>,
}

fn installed_model_views(
    catalog: &Catalog,
    installs: Vec<ModelInstall>,
    models_root: &std::path::Path,
) -> Vec<InstalledModelView> {
    installs
        .into_iter()
        .map(|install| {
            let missing_files = catalog
                .models
                .iter()
                .find(|entry| entry.id == install.model_id)
                .map(|entry| model_manager::missing_files(entry, &models_root.join(&install.path)))
                .unwrap_or_default();
            InstalledModelView {
                install,
                missing_files,
            }
        })
        .collect()
}

#[tauri::command]
async fn list_installed_models(
    manager: tauri::State<'_, Arc<ModelManager>>,
) -> Result<Vec<InstalledModelView>, String> {
    let installs = manager.list_installed().await.map_err(|e| e.to_string())?;
    let models_root = manager.models_root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        installed_model_views(catalog::bundled_catalog(), installs, &models_root)
    })
    .await
    .map_err(|e| e.to_string())
}

const DEFAULT_MODEL_PREFIX: &str = "default_model.";
const DEFAULT_MODEL_TASKS: [&str; 3] = ["llm", "tts", "align"];

#[tauri::command]
async fn get_default_models(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<HashMap<String, String>, String> {
    let rows = storage::settings::list_settings_with_prefix(&pool, DEFAULT_MODEL_PREFIX)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(key, value)| (key[DEFAULT_MODEL_PREFIX.len()..].to_string(), value))
        .collect())
}

#[tauri::command]
async fn set_default_model(
    pool: tauri::State<'_, storage::SqlitePool>,
    task: String,
    model_id: Option<String>,
) -> Result<(), String> {
    if !DEFAULT_MODEL_TASKS.contains(&task.as_str()) {
        return Err(format!("unknown default-model task '{task}'"));
    }
    let key = format!("{DEFAULT_MODEL_PREFIX}{task}");
    match model_id {
        Some(model_id) => {
            let entry = catalog::bundled_catalog()
                .models
                .iter()
                .find(|m| m.id == model_id)
                .ok_or_else(|| format!("model '{model_id}' not found in catalog"))?;
            let entry_task = model_manager::task_dir_name(entry.task);
            if entry_task != task {
                return Err(format!(
                    "model '{model_id}' is a {entry_task} model, not {task}"
                ));
            }
            storage::settings::set_setting(&pool, &key, &model_id)
                .await
                .map_err(|e| e.to_string())
        }
        None => storage::settings::delete_setting(&pool, &key)
            .await
            .map_err(|e| e.to_string()),
    }
}

const ONBOARDING_COMPLETE_KEY: &str = "onboarding_complete";

#[tauri::command]
async fn get_onboarding_complete(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<bool, String> {
    let value = storage::settings::get_setting(&pool, ONBOARDING_COMPLETE_KEY)
        .await
        .map_err(|e| e.to_string())?;
    Ok(value.as_deref() == Some("true"))
}

#[tauri::command]
async fn set_onboarding_complete(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<(), String> {
    storage::settings::set_setting(&pool, ONBOARDING_COMPLETE_KEY, "true")
        .await
        .map_err(|e| e.to_string())
}

// I18N-1: the UI language. `host_locale` is the OS-level answer (LC_ALL /
// LC_MESSAGES / LANG on Linux, the user default locale on Windows and macOS)
// as a BCP-47 tag; the webview's `navigator.language` is only the fallback
// the frontend uses when this command is unavailable (browser harness).
const LOCALE_PREFERENCE_KEY: &str = "locale_preference";
const LOCALE_PREFERENCES: [&str; 3] = ["system", "en", "ar"];

#[tauri::command]
fn host_locale() -> Option<String> {
    sys_locale::get_locale()
}

#[tauri::command]
async fn get_locale_preference(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<String, String> {
    let value = storage::settings::get_setting(&pool, LOCALE_PREFERENCE_KEY)
        .await
        .map_err(|e| e.to_string())?;
    Ok(match value {
        Some(stored) if LOCALE_PREFERENCES.contains(&stored.as_str()) => stored,
        _ => "system".to_string(),
    })
}

#[tauri::command]
async fn set_locale_preference(
    pool: tauri::State<'_, storage::SqlitePool>,
    preference: String,
) -> Result<(), String> {
    if !LOCALE_PREFERENCES.contains(&preference.as_str()) {
        return Err(format!(
            "unknown locale preference '{preference}'; expected system, en or ar"
        ));
    }
    storage::settings::set_setting(&pool, LOCALE_PREFERENCE_KEY, &preference)
        .await
        .map_err(|e| e.to_string())
}

const THEME_PREFERENCE_KEY: &str = "theme_preference";
const THEME_PREFERENCES: [&str; 3] = ["system", "light", "dark"];

#[tauri::command]
async fn get_theme_preference(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<String, String> {
    let value = storage::settings::get_setting(&pool, THEME_PREFERENCE_KEY)
        .await
        .map_err(|e| e.to_string())?;
    Ok(match value {
        Some(stored) if THEME_PREFERENCES.contains(&stored.as_str()) => stored,
        _ => "system".to_string(),
    })
}

#[tauri::command]
async fn set_theme_preference(
    pool: tauri::State<'_, storage::SqlitePool>,
    preference: String,
) -> Result<(), String> {
    if !THEME_PREFERENCES.contains(&preference.as_str()) {
        return Err(format!(
            "unknown theme preference '{preference}'; expected system, light or dark"
        ));
    }
    storage::settings::set_setting(&pool, THEME_PREFERENCE_KEY, &preference)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_ingest_fallback_enabled(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<bool, String> {
    Ok(ingest_fallback::fallback_enabled(&pool).await)
}

#[tauri::command]
async fn set_ingest_fallback_enabled(
    pool: tauri::State<'_, storage::SqlitePool>,
    enabled: bool,
) -> Result<(), String> {
    let value = if enabled { "true" } else { "false" };
    storage::settings::set_setting(&pool, ingest_fallback::FALLBACK_ENABLED_KEY, value)
        .await
        .map_err(|e| e.to_string())
}

fn main_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    app.get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())
}

#[tauri::command]
fn set_window_background(
    app: tauri::AppHandle,
    red: u8,
    green: u8,
    blue: u8,
) -> Result<(), String> {
    main_window(&app)?
        .set_background_color(Some(tauri::window::Color(red, green, blue, 255)))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_window_chrome(
    app: tauri::AppHandle,
    chrome: tauri::State<'_, window_chrome::ChromeState>,
    palette: window_chrome::ChromePalette,
) -> Result<(), String> {
    let window = main_window(&app)?;
    chrome.replace(palette);
    window_chrome::apply(&window, palette, window.is_focused().unwrap_or(true));
    Ok(())
}

#[tauri::command]
fn reveal_main_window(app: tauri::AppHandle) -> Result<(), String> {
    main_window(&app)?.show().map_err(|e| e.to_string())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageDirView {
    path: String,
    default_path: String,
    is_default: bool,
    configured: Option<String>,
    rejected: Option<String>,
}

impl StorageDirView {
    fn new(resolved: &storage::paths::ResolvedDir, default_path: &std::path::Path) -> Self {
        Self {
            path: resolved.path.to_string_lossy().into_owned(),
            default_path: default_path.to_string_lossy().into_owned(),
            is_default: resolved.is_default(),
            configured: resolved.configured.clone(),
            rejected: resolved.rejected.as_ref().map(|r| r.to_string()),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageSettings {
    models: StorageDirView,
    exports: StorageDirView,
    cache_dir: String,
    cache_bytes: i64,
    cache_cap_bytes: Option<i64>,
    cache_cap_source: &'static str,
    cache_cap_refused: Option<String>,
    default_cache_cap_bytes: i64,
}

#[tauri::command]
async fn get_storage_settings(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<StorageSettings, String> {
    read_storage_settings(&app, &pool).await
}

async fn read_storage_settings(
    app: &tauri::AppHandle,
    pool: &storage::SqlitePool,
) -> Result<StorageSettings, String> {
    let data_dir = storage::paths::resolve_app_data_dir(app)?;
    let dirs = storage_dirs(app, pool).await?;
    let fallbacks = storage::paths::defaults_with_cache(&data_dir, dirs.cache_dir.clone());
    let cap = cache_gc::read_cap(pool).await.map_err(|e| e.to_string())?;
    Ok(StorageSettings {
        models: StorageDirView::new(&dirs.models, fallbacks.models_dir()),
        exports: StorageDirView::new(&dirs.exports, fallbacks.exports_dir()),
        cache_dir: dirs.cache_dir.to_string_lossy().into_owned(),
        cache_bytes: storage::cache::total_bytes(pool)
            .await
            .map_err(|e| e.to_string())?,
        cache_cap_bytes: cap.effective_bytes(),
        cache_cap_source: cap_source_name(&cap),
        cache_cap_refused: match &cap {
            cache_gc::CapSource::Refused { raw } => Some(raw.clone()),
            _ => None,
        },
        default_cache_cap_bytes: cache_gc::DEFAULT_CAP_BYTES,
    })
}

fn cap_source_name(cap: &cache_gc::CapSource) -> &'static str {
    match cap {
        cache_gc::CapSource::Default | cache_gc::CapSource::Refused { .. } => "default",
        cache_gc::CapSource::Unlimited => "unlimited",
        cache_gc::CapSource::Configured(_) => "custom",
    }
}

#[tauri::command]
async fn set_cache_cap_bytes(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    cap_bytes: Option<i64>,
) -> Result<StorageSettings, String> {
    match cap_bytes {
        Some(cap) => cache_gc::write_cap_bytes(&pool, cap).await?,
        None => cache_gc::clear_cap(&pool).await?,
    }
    read_storage_settings(&app, &pool).await
}

#[tauri::command]
async fn delete_model(
    manager: tauri::State<'_, Arc<ModelManager>>,
    pool: tauri::State<'_, storage::SqlitePool>,
    model_id: String,
) -> Result<(), String> {
    match manager.delete(&model_id).await {
        Ok(()) | Err(model_manager::ModelManagerError::NotInstalled(_)) => {}
        Err(error) => return Err(error.to_string()),
    }
    storage::settings::delete_settings_with_value(&pool, DEFAULT_MODEL_PREFIX, &model_id)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageBreakdown {
    pub total_bytes: u64,
    pub engine: EngineStorageInfo,
    pub models: Vec<ModelStorageInfo>,
    pub cache: CacheStorageInfo,
    pub books: BooksStorageInfo,
    pub exports: ExportsStorageInfo,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStorageInfo {
    pub status: String,
    pub disk_bytes: u64,
    pub path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStorageInfo {
    pub model_id: String,
    pub name: String,
    pub task: String,
    pub disk_bytes: u64,
    pub installed_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStorageInfo {
    pub disk_bytes: u64,
    pub active_job_locks: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BooksStorageInfo {
    pub count: usize,
    pub disk_bytes: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportsStorageInfo {
    pub count: usize,
    pub disk_bytes: u64,
}

fn dir_size(path: &std::path::Path) -> u64 {
    dir_size_bounded(path, 0, 32)
}

fn dir_size_bounded(path: &std::path::Path, depth: usize, max_depth: usize) -> u64 {
    if depth > max_depth {
        return 0;
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return meta.len();
    }
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if ft.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if ft.is_dir() {
                total += dir_size_bounded(&entry.path(), depth + 1, max_depth);
            }
        }
    }
    total
}

#[tauri::command]
async fn get_storage_breakdown(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    supervisor: tauri::State<'_, SupervisorHandle>,
) -> Result<StorageBreakdown, String> {
    let data_dir = storage::paths::resolve_app_data_dir(&app)?;
    let cache_dir = storage::paths::resolve_app_cache_dir(&app)?;
    let dirs = storage_dirs(&app, &pool).await?;

    let engine_dir = data_dir.join("engine");
    let books_dir = data_dir.join("books");
    let exports_dir = dirs.exports_dir().to_path_buf();

    let installed_models = storage::models::list_model_installs(&pool)
        .await
        .map_err(|e| e.to_string())?;

    let engine_dir_scan = engine_dir.clone();
    let engine_partial_scan = data_dir.join("engine.partial");
    let cache_dir_scan = cache_dir.clone();
    let books_dir_scan = books_dir.clone();
    let exports_dir_scan = exports_dir.clone();
    let model_paths: Vec<(String, Option<std::path::PathBuf>, u64)> = installed_models
        .iter()
        .map(|install| {
            let p = std::path::PathBuf::from(&install.path);
            let p_opt = if p.exists() { Some(p) } else { None };
            (
                install.model_id.clone(),
                p_opt,
                install.disk_bytes.max(0) as u64,
            )
        })
        .collect();

    let (engine_bytes, cache_bytes, books_bytes, exports_bytes, model_sizes) =
        tokio::task::spawn_blocking(move || {
            let engine = dir_size(&engine_dir_scan) + dir_size(&engine_partial_scan);
            let cache = dir_size(&cache_dir_scan);
            let books = dir_size(&books_dir_scan);
            let exports = dir_size(&exports_dir_scan);
            let models: std::collections::HashMap<String, u64> = model_paths
                .into_iter()
                .map(|(id, path_opt, default_bytes)| {
                    let bytes = path_opt.map(|p| dir_size(&p)).unwrap_or(default_bytes);
                    (id, bytes)
                })
                .collect();
            (engine, cache, books, exports, models)
        })
        .await
        .map_err(|e| e.to_string())?;

    let status = supervisor.status();
    let status_str = match &status {
        EngineStatus::NotInstalled => "notInstalled",
        EngineStatus::NeedsUpdate { .. } => "needsUpdate",
        EngineStatus::Downloading { .. } => "downloading",
        EngineStatus::Extracting { .. } => "extracting",
        EngineStatus::Starting => "starting",
        EngineStatus::Ready { .. } => "ready",
        EngineStatus::Restarting { .. } => "restarting",
        EngineStatus::Failed { .. } => "failed",
        EngineStatus::Down => "down",
    };
    let engine = EngineStorageInfo {
        status: status_str.to_string(),
        disk_bytes: engine_bytes,
        path: engine_dir.to_string_lossy().into_owned(),
    };

    let bundled = catalog::bundled_catalog();
    let mut models = Vec::new();
    let mut models_total_bytes = 0u64;
    for install in installed_models {
        let bytes = model_sizes.get(&install.model_id).copied().unwrap_or(0);
        models_total_bytes += bytes;
        let entry = bundled.models.iter().find(|m| m.id == install.model_id);
        let name = entry
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| install.model_id.clone());
        let task = entry
            .map(|m| match m.task {
                catalog::ModelTask::Llm => "llm".to_string(),
                catalog::ModelTask::Tts => "tts".to_string(),
                catalog::ModelTask::Align => "align".to_string(),
                catalog::ModelTask::Layout => "layout".to_string(),
                catalog::ModelTask::Unknown => "unknown".to_string(),
            })
            .unwrap_or_else(|| "custom".to_string());
        models.push(ModelStorageInfo {
            model_id: install.model_id,
            name,
            task,
            disk_bytes: bytes,
            installed_at: Some(install.installed_at),
        });
    }

    let active_job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM render_job WHERE UPPER(state) NOT IN ('DONE', 'FAILED', 'CANCELLED', 'CANCELED')")
        .fetch_one(&*pool)
        .await
        .unwrap_or(0);
    let cache = CacheStorageInfo {
        disk_bytes: cache_bytes,
        active_job_locks: active_job_count as usize,
    };

    let book_count = storage::book::list_books(&pool)
        .await
        .map(|b| b.len())
        .unwrap_or(0);
    let books = BooksStorageInfo {
        count: book_count,
        disk_bytes: books_bytes,
    };

    let export_count = storage::export::list_exports(&pool)
        .await
        .map(|e| e.len())
        .unwrap_or(0);
    let exports = ExportsStorageInfo {
        count: export_count,
        disk_bytes: exports_bytes,
    };

    let total_bytes = engine_bytes + models_total_bytes + cache_bytes + books_bytes + exports_bytes;

    Ok(StorageBreakdown {
        total_bytes,
        engine,
        models,
        cache,
        books,
        exports,
    })
}

async fn remove_engine_dir(engine_dir: std::path::PathBuf) -> Result<(), String> {
    if !engine_dir.exists() {
        return Ok(());
    }
    tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(&engine_dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.to_string()),
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn uninstall_engine(
    app: tauri::AppHandle,
    supervisor: tauri::State<'_, SupervisorHandle>,
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<(), String> {
    let _gate = match app.try_state::<orchestrator::JobOrchestrator>() {
        Some(orchestrator) => Some(orchestrator.lock_mutation_gate_owned().await),
        None => None,
    };
    let active_job_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM render_job WHERE UPPER(state) NOT IN ('DONE', 'FAILED', 'CANCELLED', 'CANCELED')",
    )
    .fetch_one(&*pool)
    .await
    .map_err(|e| e.to_string())?;
    if active_job_count > 0 {
        return Err("Cannot uninstall engine while render jobs are active".to_string());
    }

    supervisor.request_engine_shutdown().await;
    supervisor
        .stop_sidecar(std::time::Duration::from_secs(3))
        .await;
    let data_dir = storage::paths::resolve_app_data_dir(&app)?;
    remove_engine_dir(data_dir.join("engine")).await?;
    remove_engine_dir(data_dir.join("engine.partial")).await?;
    SupervisorHandle::publish_status(&app, EngineStatus::NotInstalled);
    Ok(())
}

#[tauri::command]
async fn purge_audio_cache(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<u64, String> {
    let orchestrator = app.try_state::<orchestrator::JobOrchestrator>();
    let _mutation_guard = match orchestrator {
        Some(ref orch) => Some(orch.lock_mutation_gate().await),
        None => None,
    };

    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM render_job WHERE UPPER(state) NOT IN ('DONE', 'FAILED', 'CANCELLED', 'CANCELED')",
    )
    .fetch_one(&*pool)
    .await
    .map_err(|e| e.to_string())?;
    if active_count > 0 {
        return Err("Cannot purge audio cache while render jobs are active".to_string());
    }
    let cache_dir = storage::paths::resolve_app_cache_dir(&app)?;
    let cache_dir_clone = cache_dir.clone();
    let legacy_cache_opt = storage::paths::resolve_app_data_dir(&app)
        .ok()
        .map(|d| d.join(storage::paths::CACHE_DIR_NAME))
        .filter(|p| p.exists());

    let freed_bytes = tokio::task::spawn_blocking(move || {
        let mut freed = dir_size(&cache_dir_clone);
        if cache_dir_clone.exists() {
            let _ = std::fs::remove_dir_all(&cache_dir_clone);
            let _ = storage::paths::ensure_private_dir(&cache_dir_clone);
        }
        if let Some(ref legacy_cache) = legacy_cache_opt {
            freed += dir_size(legacy_cache);
            let _ = std::fs::remove_dir_all(legacy_cache);
        }
        freed
    })
    .await
    .map_err(|e| e.to_string())?;
    let _ = sqlx::query("DELETE FROM cache_entry").execute(&*pool).await;
    let _ = sqlx::query("DELETE FROM cache_ref").execute(&*pool).await;
    let _ = sqlx::query("DELETE FROM provider_request_cache")
        .execute(&*pool)
        .await;
    storage::job::reconcile_purged_cache_jobs(&pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(freed_bytes)
}

fn erase_desktop_integrations() {
    let mut data_dirs = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from) {
        data_dirs.push(xdg);
    }
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        let fallback = home.join(".local/share");
        if !data_dirs.contains(&fallback) {
            data_dirs.push(fallback);
        }
    }
    for data_dir in data_dirs {
        let apps_dir = data_dir.join("applications");
        for name in [
            "txtCaster.desktop",
            "txtcaster.desktop",
            "io.github.meedoomostafa.txtcaster.desktop",
        ] {
            let file = apps_dir.join(name);
            if file.exists() {
                let _ = std::fs::remove_file(file);
            }
        }
        let icons_base = data_dir.join("icons/hicolor");
        if icons_base.exists() {
            if let Ok(entries) = std::fs::read_dir(&icons_base) {
                for entry in entries.flatten() {
                    let apps = entry.path().join("apps");
                    if apps.exists() {
                        for icon_name in [
                            "txtcaster.png",
                            "txtCaster.png",
                            "io.github.meedoomostafa.txtcaster.png",
                        ] {
                            let icon_file = apps.join(icon_name);
                            if icon_file.exists() {
                                let _ = std::fs::remove_file(icon_file);
                            }
                        }
                    }
                }
            }
        }
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&apps_dir)
            .status();
    }
}

#[tauri::command]
async fn danger_self_purge(app: tauri::AppHandle, confirmation: String) -> Result<(), String> {
    if confirmation.trim() != "UNINSTALL" {
        return Err("Confirmation string must be 'UNINSTALL'".to_string());
    }
    tracing::warn!("Initiating danger_self_purge sequence");

    // 1. Teardown Processes
    if let Some(orchestrator) = app.try_state::<Arc<JobOrchestrator>>() {
        let _ = orchestrator
            .shutdown(std::time::Duration::from_secs(2))
            .await;
    }
    if let Some(slot) = app.try_state::<VoiceSessionSlot>() {
        let mut session = slot.0.lock().await;
        if let Some(handle) = session.as_ref() {
            let _ = handle
                .stop_and_wait(std::time::Duration::from_secs(2))
                .await;
        }
        *session = None;
    }
    if let Some(supervisor) = app.try_state::<SupervisorHandle>() {
        supervisor.request_engine_shutdown().await;
        supervisor.shutdown(std::time::Duration::from_secs(2)).await;
    }

    // 2. Checkpoint WAL & Close SQLite Pool
    if let Some(pool) = app.try_state::<storage::SqlitePool>() {
        let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&*pool)
            .await;
        pool.close().await;
    }

    // 3. Release File Locks
    if let Some(lock) = app.try_state::<SingleInstanceLock>() {
        lock.release();
    }

    // 4. Eradicate System Integration
    erase_desktop_integrations();

    // 5. Wipe Storage Directories
    if let Ok(cache_dir) = storage::paths::resolve_app_cache_dir(&app) {
        if cache_dir.exists() {
            let _ = std::fs::remove_dir_all(&cache_dir);
        }
    }
    if let Ok(base_cache) = app.path().cache_dir() {
        let legacy = base_cache.join(storage::paths::LEGACY_IDENTIFIER);
        if legacy.exists() {
            let _ = std::fs::remove_dir_all(&legacy);
        }
    }
    if let Ok(data_dir) = storage::paths::resolve_app_data_dir(&app) {
        if data_dir.exists() {
            let _ = std::fs::remove_dir_all(&data_dir);
        }
    }
    if let Ok(base_data) = app.path().data_dir() {
        let legacy = base_data.join(storage::paths::LEGACY_IDENTIFIER);
        if legacy.exists() {
            let _ = std::fs::remove_dir_all(&legacy);
        }
    }
    if let Ok(base_config) = app.path().config_dir() {
        let canonical = base_config.join(storage::paths::CANONICAL_APP_NAME);
        if canonical.exists() {
            let _ = std::fs::remove_dir_all(&canonical);
        }
        let legacy = base_config.join(storage::paths::LEGACY_IDENTIFIER);
        if legacy.exists() {
            let _ = std::fs::remove_dir_all(&legacy);
        }
    }

    // 6. AppImage Self-Unlink
    if let Ok(appimage_path) = std::env::var("APPIMAGE") {
        let path = std::path::Path::new(&appimage_path);
        if path.is_file() {
            if let Err(err) = std::fs::remove_file(path) {
                tracing::warn!("failed to unlink AppImage at {}: {err}", path.display());
            } else {
                tracing::info!("successfully unlinked AppImage at {}", path.display());
            }
        }
    }

    // 7. Emit completion and spawn exit
    let _ = app.emit("purge_complete", ());
    tauri::async_runtime::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        std::process::exit(0);
    });

    Ok(())
}

#[tauri::command]
fn cancel_model_download(
    registry: tauri::State<'_, DownloadRegistry>,
    model_id: String,
) -> Result<(), String> {
    let handles = registry.0.lock().map_err(|e| e.to_string())?;
    match handles.get(&model_id) {
        Some(cancel) => cancel.send(true).map_err(|e| e.to_string()),
        None => Err(format!("no active download for '{model_id}'")),
    }
}

#[tauri::command]
fn download_model(
    app: tauri::AppHandle,
    manager: tauri::State<'_, Arc<ModelManager>>,
    registry: tauri::State<'_, DownloadRegistry>,
    model_id: String,
) -> Result<(), String> {
    let entry = catalog::bundled_catalog()
        .models
        .iter()
        .find(|m| m.id == model_id)
        .cloned()
        .ok_or_else(|| format!("model '{model_id}' not found in catalog"))?;

    let (cancel_tx, cancel_rx) = watch::channel(false);
    {
        let mut handles = registry.0.lock().map_err(|e| e.to_string())?;
        if handles.contains_key(&model_id) {
            return Err(format!("download already in progress for '{model_id}'"));
        }
        handles.insert(model_id.clone(), cancel_tx);
    }

    let (progress_tx, mut progress_rx) = mpsc::channel::<DownloadProgress>(256);
    let emitter = app.clone();
    let forwarder = tauri::async_runtime::spawn(async move {
        while let Some(event) = progress_rx.recv().await {
            let _ = emitter.emit("model-download-progress", &event);
        }
    });

    let manager = manager.inner().clone();
    tauri::async_runtime::spawn(async move {
        let result = manager.download(&entry, progress_tx, cancel_rx).await;
        let _ = forwarder.await;
        let payload = match result {
            Ok(()) => {
                if let Some(pool) = app.try_state::<storage::SqlitePool>() {
                    if let Ok(installs) = manager.list_installed().await {
                        if let Err(error) = voices::sync_preset_voice_profiles(
                            &pool,
                            catalog::bundled_catalog(),
                            &installs,
                            manager.models_root(),
                        )
                        .await
                        {
                            tracing::warn!("voice preset sync failed: {error}");
                        }
                    }
                }
                DownloadResult::Installed {
                    model_id: entry.id.clone(),
                }
            }
            Err(e) => DownloadResult::Failed {
                model_id: entry.id.clone(),
                error: e.to_string(),
            },
        };
        let _ = app.emit("model-download-finished", &payload);
        if let Some(registry) = app.try_state::<DownloadRegistry>() {
            if let Ok(mut handles) = registry.0.lock() {
                handles.remove(&entry.id);
            }
        }
    });
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceProviderView {
    id: &'static str,
    default_base_url: Option<&'static str>,
    capabilities: &'static [voice::keys::Capability],
}

#[tauri::command]
fn list_voice_providers() -> Vec<VoiceProviderView> {
    voice::keys::Provider::ALL
        .into_iter()
        .map(|provider| VoiceProviderView {
            id: provider.id(),
            default_base_url: provider.default_base_url(),
            capabilities: provider.capabilities(),
        })
        .collect()
}

#[tauri::command]
async fn voice_key_storage_status(
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<voice::keys::StorageStatus, String> {
    Ok(keys.storage_status().await)
}

#[tauri::command]
async fn unlock_voice_key_vault(
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    passphrase: String,
) -> Result<voice::keys::StorageStatus, String> {
    let passphrase = zeroize::Zeroizing::new(passphrase);
    keys.unlock_vault(passphrase)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn use_session_only_voice_keys(
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<voice::keys::StorageStatus, String> {
    keys.use_session_only().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_voice_keys(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<Vec<voice::keys::KeyEntryView>, String> {
    keys.list_entries(&pool).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn probe_voice_keys(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<Vec<voice::keys::KeyEntryView>, String> {
    keys.probe_all(&pool).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn test_all_voice_keys(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<Vec<voice::keys::KeyEntryView>, String> {
    voice_session::test_all_keys(&pool, &keys)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn refresh_voice_key(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    base_url: String,
) -> Result<voice::keys::KeyEntryView, String> {
    keys.refresh_one(&pool, &base_url)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_voice_key(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    provider: String,
    base_url: Option<String>,
    key: String,
    label: Option<String>,
) -> Result<voice::keys::KeyEntryView, String> {
    keys.add_key(&pool, &provider, base_url.as_deref(), key, label.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn import_voice_keys_text(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    provider: String,
    base_url: Option<String>,
    text: String,
) -> Result<voice::keys::ImportReport, String> {
    keys.import_keys(&pool, &provider, base_url.as_deref(), text)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn import_voice_keys_file(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    provider: String,
    base_url: Option<String>,
    path: String,
) -> Result<voice::keys::ImportReport, String> {
    keys.import_keys_from_file(&pool, &provider, base_url.as_deref(), path.into())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn remove_voice_key(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    base_url: String,
    slot: Option<u32>,
) -> Result<(), String> {
    keys.remove_key(&pool, &base_url, slot.unwrap_or(0))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn check_voice_url(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    url: String,
) -> Result<voice::keys::UrlCheck, String> {
    keys.check_url(&pool, &url).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn voice_tutor_readiness(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<voice::keys::manager::TutorKeyReadiness, String> {
    keys.tutor_readiness(&pool)
        .await
        .map_err(|_| "Could not read tutor credential availability".to_owned())
}

#[tauri::command]
async fn list_cast_key_pool(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<voice::keys::manager::CastKeyPool, String> {
    keys.cast_key_pool(&pool).await.map_err(|e| e.to_string())
}

/// The §11.1 reverse-anchor consent gate, per (book, endpoint) — the check
/// the tap path runs before anything leaves the device; unconsented means
/// taps send nothing.
#[tauri::command]
async fn check_reverse_anchor_consent(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    book_id: String,
    url: String,
) -> Result<voice::keys::UrlCheck, String> {
    keys.check_reverse_anchor_consent(&pool, &book_id, &url)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn record_reverse_anchor_consent(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    book_id: String,
    url: String,
) -> Result<String, String> {
    keys.record_reverse_anchor_consent(&pool, &book_id, &url)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn revoke_reverse_anchor_consent(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    book_id: String,
    url: String,
) -> Result<(), String> {
    keys.revoke_reverse_anchor_consent(&pool, &book_id, &url)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_reverse_anchor_consents(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<Vec<voice::keys::ReverseAnchorConsent>, String> {
    keys.list_reverse_anchor_consents(&pool)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_learning_preferences(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<Vec<storage::learning_profile::ProfileEntry>, String> {
    storage::learning_profile::list_entries(&pool)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn forget_learning_preference(
    pool: tauri::State<'_, storage::SqlitePool>,
    key: String,
) -> Result<Vec<storage::learning_profile::ProfileEntry>, String> {
    storage::learning_profile::delete_entry(&pool, &key)
        .await
        .map_err(|e| e.to_string())?;
    storage::learning_profile::list_entries(&pool)
        .await
        .map_err(|e| e.to_string())
}

/// What the settings surface needs to render the "Personalize Learning
/// Style & Tone" toggle (owner ruling D-2): the toggle's stored position
/// plus whether §7 never-persist is active — never-persist wins over the
/// toggle, so the UI renders it disabled with an explanatory note.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LearningPrivacy {
    personalize_enabled: bool,
    never_persist: bool,
}

async fn read_learning_privacy(pool: &storage::SqlitePool) -> Result<LearningPrivacy, String> {
    let never_persist =
        storage::settings::get_setting(pool, voice_journal::TRANSCRIPT_RETENTION_KEY)
            .await
            .map_err(|e| e.to_string())?
            .is_some_and(|value| value == voice_journal::RETENTION_NEVER);
    let personalize_enabled =
        !storage::settings::get_setting(pool, learning_profile::PERSONALIZE_LEARNING_KEY)
            .await
            .map_err(|e| e.to_string())?
            .is_some_and(|value| value == learning_profile::PERSONALIZE_OFF);
    Ok(LearningPrivacy {
        personalize_enabled,
        never_persist,
    })
}

#[tauri::command]
async fn get_learning_privacy(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<LearningPrivacy, String> {
    read_learning_privacy(&pool).await
}

/// Flip the D-2 toggle. Applies from the NEXT session
/// ([`learning_profile::ProfileMode`] is decided once at session start —
/// the journal's mid-session-change semantics).
#[tauri::command]
async fn set_personalize_learning(
    pool: tauri::State<'_, storage::SqlitePool>,
    enabled: bool,
) -> Result<LearningPrivacy, String> {
    let value = if enabled {
        learning_profile::PERSONALIZE_ON
    } else {
        learning_profile::PERSONALIZE_OFF
    };
    storage::settings::set_setting(&pool, learning_profile::PERSONALIZE_LEARNING_KEY, value)
        .await
        .map_err(|e| e.to_string())?;
    read_learning_privacy(&pool).await
}

/// The webview player's snapshot report — the inbound half of the VA6
/// playback bridge (`voice_playback` module doc). Called by
/// `voicePlaybackBridge.ts` on meaningful store changes and on its heartbeat
/// while a track is loaded.
#[tauri::command]
fn voice_playback_report(
    surface: tauri::State<'_, Arc<voice_playback::PlaybackSurface>>,
    pos_ms: u64,
    playing: bool,
    speed: f64,
    seek_epoch: Option<u64>,
) {
    surface.report(pos_ms, playing, speed, seek_epoch);
}

/// The track unloaded: the playing surface is gone until the next report
/// (voice tools answer `PlaybackUnavailable`, a retryable tool error).
#[tauri::command]
fn voice_playback_detach(surface: tauri::State<'_, Arc<voice_playback::PlaybackSurface>>) {
    surface.detach();
}

#[tauri::command]
async fn record_voice_url_consent(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    url: String,
) -> Result<String, String> {
    keys.record_consent(&pool, &url)
        .await
        .map_err(|e| e.to_string())
}

/// The app's one live voice session (`voice_session` module doc: the audio
/// device and the reading view are singular). Tokio mutex on purpose — held
/// across the whole start so two racing starts serialize instead of both
/// dialing.
#[derive(Default)]
pub struct VoiceSessionSlot(pub tokio::sync::Mutex<Option<voice_session::SessionHandle>>);

impl VoiceSessionSlot {
    /// D4/DQA-H5 at the command boundary: the live guard and the delete run
    /// under ONE hold of the slot lock, so no start can slip in between.
    pub(crate) async fn delete_session(
        &self,
        pool: &storage::SqlitePool,
        session_id: &str,
    ) -> Result<(), String> {
        let session = self.0.lock().await;
        let live = live_session_id(session.as_ref());
        history_delete(pool, session_id, live.as_deref()).await
    }

    /// Same atom for forget-scope: the live row is kept, under the lock.
    pub(crate) async fn forget_scope(
        &self,
        pool: &storage::SqlitePool,
        scope: Option<&str>,
    ) -> Result<u64, String> {
        let session = self.0.lock().await;
        let live = live_session_id(session.as_ref());
        history_forget_scope(pool, scope, live.as_deref()).await
    }
}

#[tauri::command]
async fn voice_session_active(
    slot: tauri::State<'_, VoiceSessionSlot>,
) -> Result<Option<voice_session::ActiveSession>, String> {
    let session = slot.0.lock().await;
    Ok(session
        .as_ref()
        .filter(|h| h.is_alive())
        .map(|h| h.identity()))
}

// Six of the eight are tauri State handles the framework injects; splitting
// the command would only relocate the argument list into a struct nobody
// else uses.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn voice_session_start(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    surface: tauri::State<'_, Arc<voice_playback::PlaybackSurface>>,
    slot: tauri::State<'_, VoiceSessionSlot>,
    book_id: Option<String>,
    job_id: Option<String>,
    playback_pos_ms: u64,
    resume_from: Option<String>,
) -> Result<(), String> {
    let mut session = slot.0.lock().await;
    if session.as_ref().is_some_and(|handle| handle.is_alive()) {
        return Err(voice_session::SessionStartError::AlreadyRunning.to_string());
    }
    let cache_dir = storage::paths::resolve_app_cache_dir(&app)?;
    let playback_pos_ms = voice_session::start_position(
        pool.inner(),
        resume_from.as_deref(),
        job_id.as_deref(),
        playback_pos_ms,
    )
    .await;
    // F7: a book session WITHOUT a narration job gets the in-memory book
    // cursor (positions are character offsets into the book text) — the
    // webview player has no item to speak for and must never receive an
    // offset as audio time.
    let playback: Box<dyn voice::tools::playback::PlaybackControl + Send> =
        if job_id.is_none() && book_id.is_some() {
            Box::new(voice::tools::playback::BookCursor::new(playback_pos_ms))
        } else {
            Box::new(voice_playback::WebviewPlayback::new(
                surface.inner().clone(),
                app.clone(),
            ))
        };
    let sink = Box::new(voice_session::TauriSessionSink::new(app.clone()));
    let handle = voice_session::start_session(
        pool.inner().clone(),
        keys.inner().clone(),
        voice_session::SessionRequest {
            book_id,
            job_id,
            // Investigation 2026-10-09 §3.1 (F11 ruling R1): the session
            // starts on the TEXT channel, so typed chat is answered by the
            // text model; Go Interactive switches it to the live voice model.
            provider: voice_session::SessionProvider::Text,
            playback_pos_ms,
            resume_from,
        },
        &cache_dir,
        playback,
        sink,
    )
    .await
    .map_err(|e| e.to_string())?;
    #[cfg(feature = "book-agent")]
    {
        *app.state::<agent_ipc::AgentModeTag>().0.lock().unwrap() = Some((
            crate::agent_session::SessionMode::Tutor,
            handle.mailbox_watch(),
        ));
        agent_ipc::watch_session_death(app.clone(), &handle);
    }
    *session = Some(handle);
    Ok(())
}

#[tauri::command]
async fn voice_session_stop(
    app: tauri::AppHandle,
    slot: tauri::State<'_, VoiceSessionSlot>,
) -> Result<(), String> {
    let mut session = slot.0.lock().await;
    if let Some(handle) = session.as_ref() {
        // The one ownership law for every End path (T12): reliable Stop +
        // join, handle retained in the slot until death is confirmed — the
        // legacy take-then-try_send escape (full mailbox = lost Stop,
        // untracked live socket) is gone.
        if !handle
            .stop_and_wait(crate::voice_session::SESSION_STOP_WAIT)
            .await
        {
            return Err("the live session did not shut down; try again".to_string());
        }
        *session = None;
    }
    // Whatever mode was live, its transient presentation state and the mode
    // tag die with it (charter §2.2 step 5) — under the slot lock, so the
    // tag and the slot stay one atom.
    #[cfg(feature = "book-agent")]
    {
        let _ = tauri::Emitter::emit(&app, agent_ipc::AGENT_HIGHLIGHT_CLEAR_EVENT, ());
        if let Some(tag) = app.try_state::<agent_ipc::AgentModeTag>() {
            *tag.0.lock().unwrap() = None;
        }
    }
    #[cfg(not(feature = "book-agent"))]
    let _ = app;
    drop(session);
    Ok(())
}

#[tauri::command]
async fn voice_session_send_text(
    slot: tauri::State<'_, VoiceSessionSlot>,
    text: String,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    let sent = session
        .as_ref()
        .is_some_and(|handle| handle.send(voice_session::SessionCommand::SendText { text }));
    if sent {
        Ok(())
    } else {
        Err("no live voice session".to_string())
    }
}

/// F8 D2 bidirectional (BookDirect half): the reader reports a manual page
/// turn so the session's BookCursor follows the visible page. Best-effort on
/// purpose — a turn with no live session (or one landing as the session
/// ends) is a no-op, never an error the user sees.
#[tauri::command]
async fn voice_session_page_turned(
    slot: tauri::State<'_, VoiceSessionSlot>,
    session_id: String,
    book_id: String,
    job_id: Option<String>,
    page_no: i64,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    if let Some(handle) = session
        .as_ref()
        .filter(|h| h.matches_reader(&session_id, &book_id, job_id.as_deref()))
    {
        handle.send(voice_session::SessionCommand::PageTurned { page_no });
    }
    Ok(())
}

#[tauri::command]
async fn voice_session_page_shown(
    slot: tauri::State<'_, VoiceSessionSlot>,
    session_id: String,
    book_id: String,
    job_id: Option<String>,
    page_no: Option<i64>,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    if let Some(handle) = session
        .as_ref()
        .filter(|h| h.matches_reader(&session_id, &book_id, job_id.as_deref()))
    {
        handle.send(voice_session::SessionCommand::PageShown(page_no));
    }
    Ok(())
}

#[tauri::command]
async fn voice_session_navigation_ack(
    slot: tauri::State<'_, VoiceSessionSlot>,
    session_id: String,
    call_id: String,
    generation: u64,
    page_no: i64,
    disposition: voice_session::navigation::NavigationDisposition,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    if let Some(handle) = session.as_ref().filter(|h| h.session_id() == session_id) {
        if !handle.send(voice_session::SessionCommand::NavigationAck {
            call_id,
            generation,
            page_no,
            disposition,
        }) {
            return Err("the tutor session is no longer available".into());
        }
    }
    Ok(())
}

#[tauri::command]
async fn voice_session_interrupt(slot: tauri::State<'_, VoiceSessionSlot>) -> Result<(), String> {
    let session = slot.0.lock().await;
    // Best-effort on purpose: interrupting a session that just ended is a
    // no-op, not an error the user needs to see.
    if let Some(handle) = session.as_ref() {
        handle.send(voice_session::SessionCommand::Interrupt);
    }
    Ok(())
}

#[tauri::command]
async fn voice_session_enter_interactive(
    slot: tauri::State<'_, VoiceSessionSlot>,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    // A deliberate mode change with nothing to change fails loudly (the
    // send_text shape), unlike best-effort interrupt.
    let sent = session
        .as_ref()
        .is_some_and(|handle| handle.send(voice_session::SessionCommand::EnterInteractive));
    if sent {
        Ok(())
    } else {
        Err("no live voice session".to_string())
    }
}

/// Back to typing: a text-first session on voice returns to the text
/// model, carrying the conversation. Fails loudly with no session, like
/// Go Interactive.
#[tauri::command]
async fn voice_session_leave_interactive(
    slot: tauri::State<'_, VoiceSessionSlot>,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    let sent = session
        .as_ref()
        .is_some_and(|handle| handle.send(voice_session::SessionCommand::LeaveInteractive));
    if sent {
        Ok(())
    } else {
        Err("no live voice session".to_string())
    }
}

/// Everything the Cast form's provider pickers need (pre-brief item 3):
/// rows exist only for phases with an implemented adapter AND at least one
/// enabled compatible connection; zero connections = the schema-identical
/// local-only form (T7). TTS options are the cloud voice profiles (A15).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CastProviderOptions {
    script_connections: Vec<CastConnectionOption>,
    tts_voices: Vec<CastCloudVoiceOption>,
    vision_connections: Vec<CastConnectionOption>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CastConnectionOption {
    #[serde(skip_serializing_if = "Option::is_none")]
    model_vision: Option<std::collections::BTreeMap<String, providers::discovery::Support>>,
    connection_id: String,
    name: String,
    provider: String,
    default_model: String,
    loopback: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CastCloudVoiceOption {
    voice_profile_id: String,
    name: String,
    voice_id: String,
    connection_id: String,
    connection_name: String,
    provider: String,
    model: String,
    loopback: bool,
}

#[tauri::command]
async fn cast_provider_options(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<CastProviderOptions, String> {
    let records = providers::ConnectionStore::list(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let mut model_vision = std::collections::HashMap::new();
    for record in &records {
        if let Ok(Some(catalog)) =
            providers::setup::cached_catalog(&pool, providers::directory::identity(record)).await
        {
            model_vision.insert(
                record.id.clone(),
                catalog
                    .models
                    .into_iter()
                    .map(|model| (model.id, model.vision))
                    .collect::<std::collections::BTreeMap<_, _>>(),
            );
        }
    }
    let script_connections = records
        .iter()
        .filter(|record| record.enabled)
        .filter(|record| {
            record
                .provider
                .servable_phases()
                .contains(&providers::PhaseCapability::ScriptLlm)
        })
        .map(|record| CastConnectionOption {
            model_vision: model_vision.get(&record.id).cloned(),
            connection_id: record.id.clone(),
            name: record.name.clone(),
            provider: record.provider.id().to_string(),
            default_model: record.default_model.clone(),
            loopback: voice::keys::policy::is_loopback_base_url(&record.key_base_url()),
        })
        .collect();
    let profiles = storage::voice::list_voice_profiles(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let mut tts_voices = Vec::new();
    for profile in profiles
        .iter()
        .filter(|profile| profile.engine == providers::CLOUD_VOICE_ENGINE)
    {
        // The one canonical validator (Hermes R2): stranded/malformed cloud
        // profiles never render as selectable.
        let Ok(valid) = providers::validate_cloud_profile(
            &profile.engine,
            &profile.kind,
            profile.reference_wav_path.as_deref(),
            profile.preset_voice_id.as_deref(),
            profile.params_json.as_deref(),
        ) else {
            continue;
        };
        let Some(record) = records
            .iter()
            .find(|record| record.id == valid.connection_id && record.enabled)
        else {
            continue;
        };
        if !record
            .provider
            .servable_phases()
            .contains(&providers::PhaseCapability::TtsSynth)
        {
            continue;
        }
        tts_voices.push(CastCloudVoiceOption {
            voice_profile_id: profile.id.clone(),
            name: profile.name.clone(),
            voice_id: valid.voice_id,
            connection_id: record.id.clone(),
            connection_name: record.name.clone(),
            provider: record.provider.id().to_string(),
            model: providers::setup::narration_model(&pool, record).await?,
            loopback: voice::keys::policy::is_loopback_base_url(&record.key_base_url()),
        });
    }
    let vision_connections = records
        .iter()
        .filter(|record| record.enabled)
        .filter(|record| {
            record
                .provider
                .servable_phases()
                .contains(&providers::PhaseCapability::VisionMllm)
        })
        .map(|record| CastConnectionOption {
            model_vision: model_vision.get(&record.id).cloned(),
            connection_id: record.id.clone(),
            name: record.name.clone(),
            provider: record.provider.id().to_string(),
            default_model: record.default_model.clone(),
            loopback: voice::keys::policy::is_loopback_base_url(&record.key_base_url()),
        })
        .collect();
    Ok(CastProviderOptions {
        script_connections,
        tts_voices,
        vision_connections,
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FetchedModels {
    models: Vec<String>,
    suggested_default: Option<String>,
}

/// The per-connection fetched-model cache (S-5 blocker 7): a successful
/// explicit fetch is remembered with its time, so the sub-page can render
/// the cached list with an honest "fetched N days ago" stale fact instead
/// of refetching on entry (T11: fetch stays user-triggered).
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedModels {
    models: Vec<String>,
    suggested_default: Option<String>,
    fetched_unix: i64,
}

fn models_cache_key(connection_id: &str) -> String {
    format!("provider_models_cache.{connection_id}")
}

#[tauri::command]
async fn provider_cached_models(
    pool: tauri::State<'_, storage::SqlitePool>,
    connection_id: String,
) -> Result<Option<CachedModels>, String> {
    let raw = storage::settings::get_setting(&pool, &models_cache_key(&connection_id))
        .await
        .map_err(|e| e.to_string())?;
    // A cache that does not parse is stale garbage, not an error: report
    // no cache and let the user fetch fresh.
    Ok(raw.and_then(|json| serde_json::from_str(&json).ok()))
}

#[tauri::command]
fn provider_directory() -> Vec<providers::directory::ProviderDefinition> {
    providers::directory::definitions().to_vec()
}

#[tauri::command]
async fn provider_cached_catalog(
    pool: tauri::State<'_, storage::SqlitePool>,
    provider_id: String,
) -> Result<Option<providers::discovery::Catalog>, String> {
    providers::setup::cached_catalog(&pool, &provider_id).await
}

#[tauri::command]
async fn provider_connect(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    provider_id: String,
    key: Option<String>,
    operation_id: String,
) -> Result<providers::discovery::Catalog, String> {
    providers::setup::connect(&pool, &keys, &provider_id, key, &operation_id, |phase| {
        let _ = app.emit(
            "provider-setup-progress",
            serde_json::json!({"operationId":operation_id,"phase":phase}),
        );
    })
    .await
}

#[tauri::command]
async fn provider_refresh_catalog(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    provider_id: String,
    operation_id: Option<String>,
    continue_listing: Option<bool>,
) -> Result<providers::discovery::Catalog, String> {
    providers::setup::refresh(
        &pool,
        &keys,
        &provider_id,
        operation_id.as_deref(),
        continue_listing.unwrap_or(false),
    )
    .await
}

#[tauri::command]
async fn provider_apply_recommendations(
    pool: tauri::State<'_, storage::SqlitePool>,
    provider_id: String,
    revision: String,
    selected: Option<providers::discovery::Recommendations>,
) -> Result<(), String> {
    providers::setup::apply(&pool, &provider_id, &revision, selected).await
}

#[tauri::command]
fn provider_cancel_setup(operation_id: String) {
    providers::setup::cancel(&operation_id);
}

/// Explicit, user-triggered model listing for one connection (T11: never
/// background; T5: suggestions only, manual entry stands). The suggested
/// default follows the owner's tier policy (latest flash / latest sonnet /
/// latest-then-cheapest).
#[tauri::command]
async fn provider_fetch_models(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    connection_id: String,
) -> Result<FetchedModels, String> {
    let connection = providers::ConnectionStore::get(&pool, &connection_id)
        .await
        .map_err(|e| e.to_string())?;
    let (models, suggested_default) =
        providers::setup::models_for_connection(&pool, &keys, &connection).await?;
    let cached = CachedModels {
        models: models.clone(),
        suggested_default: suggested_default.clone(),
        fetched_unix: chrono_free_unix_now(),
    };
    if let Ok(json) = serde_json::to_string(&cached) {
        if let Err(error) =
            storage::settings::set_setting(&pool, &models_cache_key(&connection_id), &json).await
        {
            tracing::warn!("model cache not persisted: {error}");
        }
    }
    Ok(FetchedModels {
        models,
        suggested_default,
    })
}

/// Pre-creation model listing for the add-connection form (owner feedback
/// F1, 2026-08-29): same validation, custody, and egress seam as the saved
/// path — `ConnectionRecord::draft` runs the store's own validation
/// (canonical URL, loopback-only keyless) and the fetch rides
/// `fetch_models_gated`'s PF-PH1 pool custody. No cache row: drafts have
/// no connection id, so the result lives only in the open form. Drafts
/// are enabled by construction — the per-connection disable gate cannot
/// apply before a connection exists; F1 sanctions this listing class.
#[tauri::command]
async fn provider_draft_models(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    provider_type: String,
    base_url: Option<String>,
    credential_not_required: bool,
) -> Result<FetchedModels, String> {
    let draft = providers::ConnectionRecord::draft(
        &provider_type,
        base_url.as_deref(),
        credential_not_required,
    )
    .map_err(|e| e.to_string())?;
    let (models, suggested_default) =
        providers::setup::models_for_connection(&pool, &keys, &draft).await?;
    Ok(FetchedModels {
        models,
        suggested_default,
    })
}

#[tauri::command]
async fn estimate_cast_cost(
    pool: tauri::State<'_, storage::SqlitePool>,
    phase: String,
    connection_id: String,
    model: Option<String>,
    book_id: String,
    scope: Scope,
    mode: String,
) -> Result<providers::cost::CostEstimate, String> {
    let connection = providers::ConnectionStore::get(&pool, &connection_id)
        .await
        .map_err(|e| e.to_string())?;
    let model = model.unwrap_or_else(|| connection.default_model.clone());
    let today = providers::cost::iso_date_from_unix(storage::now_ms() / 1000);
    // The preview uses the same authoritative basis the durable snapshot
    // will (Hermes R2 fold-in 5) — never a frontend-supplied figure.
    let source_chars = providers::cast_plan::scoped_source_chars(&pool, &book_id, &scope).await?;
    Ok(match phase.as_str() {
        "script" => providers::cost::estimate_script(
            providers::cost::pricing_provider(&connection),
            &model,
            source_chars,
            &today,
        ),
        _ => providers::cost::estimate_tts(
            connection.provider.id(),
            &model,
            source_chars,
            mode == "teacher",
            &today,
        ),
    })
}

/// S-5 Settings → Providers projection: one backend-derived view for the
/// top-level cards and each provider sub-page. Typed counts + computed
/// labels only — React never infers health, and no key bytes exist in the
/// returned types.
#[tauri::command]
async fn providers_overview(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<Vec<providers::overview::ProviderFamilyView>, String> {
    providers::overview::providers_overview(&pool, &keys).await
}

/// PF-PH1 `Disabled`: operator-disable/enable one slot. The slot stays
/// configured; it leaves the operational population and rotation.
#[tauri::command]
async fn provider_slot_set_disabled(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    account: String,
    disabled: bool,
) -> Result<(), String> {
    keys.set_slot_disabled(&pool, &account, disabled)
        .await
        .map_err(|e| e.to_string())
}

/// Explicit pool test (pre-brief sub-page rule 3): probes ONLY this
/// provider's slots at this canonical URL, sequentially bounded (never 50
/// simultaneous probes), emitting typed progress events the page renders
/// as rule-4 Checking. Page entry and this command are the only probe
/// triggers; there is no ambient polling.
#[tauri::command]
async fn provider_pool_test(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    provider: String,
    base_url: String,
) -> Result<(), String> {
    use tauri::Emitter;
    let entries = keys.list_entries(&pool).await.map_err(|e| e.to_string())?;
    let targets: Vec<String> = entries
        .into_iter()
        .filter(|e| e.provider == provider && e.base_url == base_url)
        .map(|e| e.account)
        .collect();
    let total = targets.len() as u32;
    // The manager's registry is the ONE progress truth: the overview
    // projects it as rule-4 Checking, this command emits the SAME typed
    // shape as events, and `provider_pool_test_cancel` flips its flag —
    // the cooperative check between probes stops the run (S-5 blocker 6).
    // A refused begin means a test is already running for this pool: the
    // second invocation must not overwrite or cancel the first run's truth.
    if !keys.begin_pool_test(&provider, &base_url, total) {
        return Err(format!(
            "a pool test is already running for {provider} at {base_url}; \
             wait for it to finish or cancel it first"
        ));
    }
    let emit = |completed: u32, done: bool, cancelled: bool| {
        let _ = app.emit(
            providers::overview::POOL_TEST_PROGRESS_EVENT,
            providers::overview::PoolTestProgress {
                provider: provider.clone(),
                base_url: base_url.clone(),
                completed,
                total,
                done,
                cancelled,
            },
        );
    };
    emit(0, false, false);
    // `probed` is the terminal truth: exactly how many probes SETTLED.
    // A cancelled run reports that count, never `total` (re-verdict item 1).
    let mut probed = 0u32;
    let mut cancelled = false;
    for account in targets {
        if !keys.update_pool_test(&provider, &base_url, probed, Some(&account)) {
            cancelled = true;
            break;
        }
        let _ = keys.refresh_one(&pool, &account).await;
        probed += 1;
        let live = keys.update_pool_test(&provider, &base_url, probed, None);
        emit(probed, false, false);
        if !live {
            cancelled = true;
            break;
        }
    }
    keys.finish_pool_test(&provider, &base_url);
    emit(probed, true, cancelled);
    Ok(())
}

/// Cooperatively cancel an in-flight explicit pool test: the probe in
/// flight completes, then the run stops and reports done+cancelled.
#[tauri::command]
async fn provider_pool_test_cancel(
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
    provider: String,
    base_url: String,
) -> Result<(), String> {
    keys.cancel_pool_test(&provider, &base_url);
    Ok(())
}

/// S-5 blocker 8: the production entry for cloud voices (A15 Option 3).
/// Creates a `voice_profile` row with engine `cloud`, kind `preset`, the
/// provider voice id, and the structural `connection_id` reference —
/// validated through the ONE canonical cloud-profile validator before it
/// is written, so nothing unservable ever lands.
#[tauri::command]
async fn create_cloud_voice_profile(
    pool: tauri::State<'_, storage::SqlitePool>,
    connection_id: String,
    name: String,
    voice_id: String,
) -> Result<VoiceProfileSummary, String> {
    let _boundary = providers::cast_plan::provider_mutation_mutex().lock().await;
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Give this voice a name so you can find it later.".to_string());
    }
    let voice = voice_id.trim();
    if voice.is_empty() {
        return Err("Enter the provider's voice id.".to_string());
    }
    let connection = providers::ConnectionStore::get(&pool, &connection_id)
        .await
        .map_err(|e| e.to_string())?;
    if !connection.enabled {
        return Err(format!(
            "connection '{}' is disabled; enable it before adding voices",
            connection.name
        ));
    }
    if !connection
        .provider
        .servable_phases()
        .contains(&providers::PhaseCapability::TtsSynth)
    {
        return Err(format!(
            "provider '{}' cannot serve audio voices",
            connection.provider.id()
        ));
    }
    let params = serde_json::json!({ "connection_id": connection.id }).to_string();
    let profile = storage::voice::VoiceProfile {
        id: storage::new_id(),
        name: trimmed.to_string(),
        engine: providers::CLOUD_VOICE_ENGINE.to_string(),
        kind: "preset".to_string(),
        preset_voice_id: Some(voice.to_string()),
        reference_wav_path: None,
        params_json: Some(params),
        created_at: storage::now_ms(),
    };
    providers::validate_cloud_profile(
        &profile.engine,
        &profile.kind,
        profile.reference_wav_path.as_deref(),
        profile.preset_voice_id.as_deref(),
        profile.params_json.as_deref(),
    )
    .map_err(|e| format!("refusing to store an unservable cloud voice: {e}"))?;
    storage::voice::insert_voice_profile(&pool, &profile)
        .await
        .map_err(|e| e.to_string())?;
    Ok(VoiceProfileSummary {
        id: profile.id,
        name: profile.name,
        engine: profile.engine,
        kind: profile.kind,
        preset_voice_id: profile.preset_voice_id,
    })
}

/// The discoverable consent list (S-5 closes the deferral): every recorded
/// cast consent, revoked ones included — revocation is a recorded fact.
#[tauri::command]
async fn cast_consent_list(
    pool: tauri::State<'_, storage::SqlitePool>,
) -> Result<Vec<providers::consent::CastConsent>, String> {
    providers::consent::ConsentStore::list(&pool)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cast_consent_grant(
    pool: tauri::State<'_, storage::SqlitePool>,
    book_id: String,
    connection_id: String,
    phase: String,
    hosts: Vec<String>,
    data_categories: Vec<String>,
) -> Result<providers::consent::CastConsent, String> {
    providers::cast_plan::grant_cast_consent(
        &pool,
        &book_id,
        &connection_id,
        &phase,
        hosts,
        data_categories,
        storage::now_ms() / 1000,
    )
    .await
}

#[tauri::command]
async fn cast_consent_revoke(
    pool: tauri::State<'_, storage::SqlitePool>,
    consent_id: String,
) -> Result<bool, String> {
    providers::cast_plan::revoke_cast_consent(&pool, &consent_id, storage::now_ms() / 1000).await
}

/// A22 pre-Start advisory (pre-brief item 11): a timestamp-less cloud voice
/// on a machine with no resolvable align model will fall back to sentence
/// highlighting — say so before Start, with the Models remedy.
#[tauri::command]
async fn cast_align_advisory(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    voice_profile_id: String,
) -> Result<bool, String> {
    let data_dir = storage::paths::resolve_app_data_dir(&app)?;
    let models_root = data_dir.join("models");
    let Some(profile) = storage::voice::get_voice_profile(&pool, &voice_profile_id)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    // The one canonical validator (Hermes R2 blocker 4): the advisory
    // reasons from the same profile facts every other consumer does.
    let Ok(valid) = providers::validate_cloud_profile(
        &profile.engine,
        &profile.kind,
        profile.reference_wav_path.as_deref(),
        profile.preset_voice_id.as_deref(),
        profile.params_json.as_deref(),
    ) else {
        return Ok(false);
    };
    let Ok(connection) = providers::ConnectionStore::get(&pool, &valid.connection_id).await else {
        return Ok(false);
    };
    let installs = storage::models::list_model_installs(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let align_setting =
        storage::settings::get_setting(&pool, orchestrator::align::DEFAULT_ALIGN_SETTING)
            .await
            .map_err(|e| e.to_string())?;
    let align_resolved = orchestrator::align::resolve_align_id(
        None,
        align_setting.as_deref(),
        catalog::bundled_catalog(),
        &installs,
        &models_root,
        None,
    )
    .is_some();
    Ok(orchestrator::synthesis_cloud::align_advisory_needed(
        connection.provider,
        align_resolved,
    ))
}

#[tauri::command]
async fn provider_connections_list(
    pool: tauri::State<'_, storage::SqlitePool>,
    keys: tauri::State<'_, Arc<voice::keys::KeyManager>>,
) -> Result<Vec<providers::ConnectionView>, String> {
    let records = providers::ConnectionStore::list(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let entries = keys.list_entries(&pool).await.map_err(|e| e.to_string())?;
    Ok(records
        .iter()
        .map(|record| {
            let custody = record.key_base_url();
            let entry = entries.iter().find(|entry| {
                entry.provider == record.provider.vault_provider().id()
                    && providers::directory::same_credential_group(
                        &entry.provider,
                        &entry.base_url,
                        &custody,
                    )
                    && entry.key_present
            });
            providers::ConnectionView::from_record(
                record,
                entry.is_some(),
                entry.and_then(|e| e.last4.clone()),
            )
        })
        .collect())
}

#[tauri::command]
async fn provider_connection_create(
    pool: tauri::State<'_, storage::SqlitePool>,
    name: String,
    provider_type: String,
    base_url: Option<String>,
    credential_not_required: Option<bool>,
    default_model: String,
) -> Result<providers::ConnectionRecord, String> {
    let _boundary = providers::cast_plan::provider_mutation_mutex().lock().await;
    providers::ConnectionStore::create(
        &pool,
        &name,
        &provider_type,
        base_url.as_deref(),
        credential_not_required.unwrap_or(false),
        &default_model,
        chrono_free_unix_now(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn provider_connection_update(
    pool: tauri::State<'_, storage::SqlitePool>,
    id: String,
    name: String,
    base_url: Option<String>,
    credential_not_required: Option<bool>,
    default_model: String,
    enabled: bool,
) -> Result<providers::ConnectionRecord, String> {
    let _boundary = providers::cast_plan::provider_mutation_mutex().lock().await;
    providers::ConnectionStore::update(
        &pool,
        &id,
        &name,
        base_url.as_deref(),
        credential_not_required.unwrap_or(false),
        &default_model,
        enabled,
        chrono_free_unix_now(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn provider_connection_delete_impact(
    pool: tauri::State<'_, storage::SqlitePool>,
    id: String,
) -> Result<providers::DeleteImpact, String> {
    providers::ConnectionStore::delete_impact(&pool, &id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn provider_connection_delete(
    pool: tauri::State<'_, storage::SqlitePool>,
    id: String,
) -> Result<(), String> {
    providers::cast_plan::delete_connection_and_consents(&pool, &id).await?;
    // Data hygiene: the fetched-model cache row is keyed by the (non-
    // reusable) connection id — deleting the connection retires it.
    storage::settings::delete_setting(&pool, &models_cache_key(&id))
        .await
        .map_err(|e| e.to_string())
}

fn chrono_free_unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The one delivery seam for the four capture-gate commands (Kritias round
/// 3 finding 2): NO session = the request is already satisfied (nothing is
/// being captured — the interrupt-on-ended contract), but a LIVE session
/// whose mailbox refuses the command is a FAILURE the caller must see. The
/// old code returned Ok either way, so the frontend could believe a mute
/// the actor never received — a privacy-adjacent lie.
fn deliver_gate_command(
    session: Option<&voice_session::SessionHandle>,
    command: voice_session::SessionCommand,
) -> Result<(), String> {
    match session {
        None => Ok(()),
        Some(handle) => match handle.try_send(command) {
            Ok(()) => Ok(()),
            Err(voice_session::SendError::SessionBusy) => Err("session_busy".to_string()),
            Err(voice_session::SendError::NoSession) => {
                Err("the voice session did not accept the command".to_string())
            }
        },
    }
}

#[tauri::command]
async fn voice_session_mute(
    slot: tauri::State<'_, VoiceSessionSlot>,
    reason: Option<String>,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    deliver_gate_command(
        session.as_ref(),
        voice_session::SessionCommand::Mute { reason },
    )
}

#[tauri::command]
async fn voice_session_unmute(
    slot: tauri::State<'_, VoiceSessionSlot>,
    reason: Option<String>,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    deliver_gate_command(
        session.as_ref(),
        voice_session::SessionCommand::Unmute { reason },
    )
}

/// R2 defect 1 (owner feedback 2026-08-30): the frontend half-duplex AUTO
/// gate. Distinct from `voice_session_mute` on purpose: a user privacy mute
/// hard-zeroes capture at the device; the auto gate soft-holds it so the
/// release can flush the gate-latency window's words.
#[tauri::command]
async fn voice_session_gate(
    slot: tauri::State<'_, VoiceSessionSlot>,
    reason: Option<String>,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    deliver_gate_command(
        session.as_ref(),
        voice_session::SessionCommand::GateCapture { reason },
    )
}

#[tauri::command]
async fn voice_session_ungate(
    slot: tauri::State<'_, VoiceSessionSlot>,
    reason: Option<String>,
) -> Result<(), String> {
    let session = slot.0.lock().await;
    deliver_gate_command(
        session.as_ref(),
        voice_session::SessionCommand::UngateCapture { reason },
    )
}

// ---------------------------------------------------------------------------
// F11-U1 — session history read path (spec §5.1/§5.2; owner rulings D4, R2).
// Local SQLite only; no provider egress on any of these paths (D7). The five
// commands below are thin wrappers over pool-level seams that take the live
// row's id explicitly, so the pins drive the seams without a tauri runtime.
// ---------------------------------------------------------------------------

/// §5.1 `position`: `"ms"` on the cast timeline (H7), `"offset"` into the
/// book text (H6), or `"none"` for a standalone row (H5).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SessionPositionDto {
    pub kind: String,
    pub value: Option<i64>,
}

/// §5.1 `SessionSummaryDto`: one history row as the surface reads it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SessionSummaryDto {
    pub id: String,
    pub book_id: Option<String>,
    pub job_id: Option<String>,
    pub started_at: i64,
    /// Stored, or derived for H4 (§5.2): the last turn, else the start.
    pub ended_at: i64,
    /// `Ended`, or `Unexpected` for an H4 orphan-live row — the two a stored
    /// row can prove (F11-U3 C1).
    pub end_reason: voice_session::EndReason,
    pub duration_ms: i64,
    pub turn_count: u64,
    /// First sentence of the rolling summary, ≤ 160 chars; `None` for H2.
    pub summary_excerpt: Option<String>,
    pub position: SessionPositionDto,
    /// Not derived in U1 (chapter-map resolution is §5.3.1 work); the UI
    /// omits the column when absent — nothing is invented here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapters_touched: Option<Vec<u32>>,
    /// §5.3.1: the job row still exists and is a cast of this book.
    pub job_resolves: bool,
    /// §5.3.1: where the stored position resolves against the row's own
    /// chapter map (F11-U3 C3); `None` when it does not — never a guess.
    pub location: Option<voice::channel::Location>,
    /// The row's provider identity differs from the current channel's key
    /// identity (one channel on main; a second provider must pass its own).
    pub provider_changed: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TranscriptTurnDto {
    pub turn_idx: i64,
    pub role: String,
    pub text: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voiceprint: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SessionDetailDto {
    pub row: SessionSummaryDto,
    pub turns: Vec<TranscriptTurnDto>,
    /// The whole rolling summary (F11-U3 C2); the row carries the excerpt.
    pub summary_text: Option<String>,
}

/// [R2] What a cast delete will cascade, and whether the live row blocks it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JobDeletePreviewDto {
    pub session_count: u64,
    pub live: bool,
}

const LIVE_ROW_ERR: &str = "session is live";
/// Deliberately does not echo the webview-supplied id.
const NOT_FOUND_ERR: &str = "session not found";
/// §5.1: the excerpt bound. Owner-tunable; a longer first sentence is clipped
/// on a word boundary and marked with an ellipsis.
const SUMMARY_EXCERPT_CHARS: usize = 160;

/// Byte index just past the first sentence: the first terminator that is
/// followed by whitespace or the end of the text (so "3.5" does not split).
fn end_of_first_sentence(text: &str) -> usize {
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?' | '؟' | '。' | '！' | '？') {
            let mut end = i + c.len_utf8();
            // A closing quote or bracket belongs to the sentence it closes.
            while let Some((j, close)) = chars.peek().copied() {
                if matches!(close, '"' | '\'' | ')' | ']' | '»' | '”' | '’') {
                    end = j + close.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            match chars.peek() {
                None => return end,
                Some((_, next)) if next.is_whitespace() => return end,
                Some(_) => {}
            }
        }
    }
    text.len()
}

/// §5.1 `summary_excerpt`: the first sentence of the rolling summary, clipped
/// at [`SUMMARY_EXCERPT_CHARS`] on a word boundary. Pure text handling; the
/// summary is model-written and untrusted, and rides to the UI as a text node.
pub(crate) fn summary_excerpt(summary_text: &str) -> String {
    let text = summary_text.trim();
    let first = text[..end_of_first_sentence(text)].trim_end();
    if first.chars().count() <= SUMMARY_EXCERPT_CHARS {
        return first.to_string();
    }
    let budget: String = first.chars().take(SUMMARY_EXCERPT_CHARS - 1).collect();
    // Clip on a word boundary unless the only boundary is degenerate (an
    // unbroken run longer than half the budget): then the hard clip wins.
    let clipped = match budget.rfind(char::is_whitespace) {
        Some(i) if i >= budget.len() / 2 => &budget[..i],
        _ => budget.as_str(),
    };
    format!("{}…", clipped.trim_end())
}

/// §5.2 derive rules over one list row. The caller has already removed the
/// live row, so a NULL `ended_at` here is an H4 orphan: it reports
/// "unexpected" and ends at its last turn (or its start). Read-only — the
/// stored row is never updated (D-9 (a): no sweep).
fn summarize_session(row: &storage::voice_journal::SessionListRow) -> SessionSummaryDto {
    let (ended_at, end_reason) = match row.ended_at {
        Some(t) => (
            t,
            voice_session::EndReason::parse(&row.end_reason)
                .unwrap_or(voice_session::EndReason::Ended),
        ),
        None => (
            row.last_turn_at.unwrap_or(row.started_at),
            voice_session::EndReason::Unexpected,
        ),
    };
    let position = match (&row.job_id, &row.book_id) {
        (Some(_), _) => SessionPositionDto {
            kind: "ms".to_string(),
            value: Some(row.playback_pos_ms),
        },
        (None, Some(_)) => SessionPositionDto {
            kind: "offset".to_string(),
            value: Some(row.playback_pos_ms),
        },
        (None, None) => SessionPositionDto {
            kind: "none".to_string(),
            value: None,
        },
    };
    SessionSummaryDto {
        id: row.id.clone(),
        book_id: row.book_id.clone(),
        job_id: row.job_id.clone(),
        started_at: row.started_at,
        ended_at,
        end_reason,
        duration_ms: ended_at.saturating_sub(row.started_at).max(0),
        turn_count: row.turn_count.max(0) as u64,
        summary_excerpt: row
            .summary_text
            .as_deref()
            .map(summary_excerpt)
            .filter(|s| !s.is_empty()),
        position,
        chapters_touched: None,
        job_resolves: row.job_resolves,
        location: None,
        provider_changed: row.provider_base_url != voice_session::GEMINI_KEY_BASE_URL,
    }
}

/// The live row's id: only an ALIVE actor holds a row (a dead handle left in
/// the slot names nothing — its row is history now).
fn live_session_id(session: Option<&voice_session::SessionHandle>) -> Option<String> {
    session
        .filter(|handle| handle.is_alive())
        .map(|handle| handle.session_id().to_string())
}

async fn history_list(
    pool: &storage::SqlitePool,
    scope: Option<&str>,
    live_id: Option<&str>,
) -> Result<Vec<SessionSummaryDto>, String> {
    // The live row is excluded in SQL so it never eats a bounded slot.
    let rows = storage::voice_journal::list_sessions(pool, scope, live_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(summarize_session).collect())
}

async fn history_transcript(
    pool: &storage::SqlitePool,
    session_id: &str,
    live_id: Option<&str>,
) -> Result<SessionDetailDto, String> {
    if live_id == Some(session_id) {
        return Err(LIVE_ROW_ERR.to_string());
    }
    let row = storage::voice_journal::list_row(pool, session_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| NOT_FOUND_ERR.to_string())?;
    let mut voiceprints = storage::voice_journal::turn_voiceprints(pool, session_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!("voice history: voiceprints unreadable: {error}");
            Default::default()
        });
    let turns = storage::voice_journal::all_turns(pool, session_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|t| TranscriptTurnDto {
            voiceprint: voiceprints.remove(&t.turn_idx),
            turn_idx: t.turn_idx,
            role: t.role,
            text: t.text,
            created_at: t.created_at,
        })
        .collect();
    Ok(SessionDetailDto {
        row: summarize_session(&row),
        turns,
        summary_text: row.summary_text.clone(),
    })
}

/// F11-U3 C3: fill `location` (and the one-chapter `chapters_touched`) on
/// every row whose map resolves — one map load per distinct cast or book,
/// never one per row. An unreadable map leaves the row as it was.
async fn attach_locations(
    pool: &storage::SqlitePool,
    cache_dir: &std::path::Path,
    rows: &mut [SessionSummaryDto],
) {
    let mut maps: std::collections::HashMap<
        (Option<String>, Option<String>),
        Option<voice::excerpt::source::SessionSnapshot>,
    > = Default::default();
    for row in rows.iter_mut() {
        let Some(value) = row.position.value else {
            continue;
        };
        let key = (row.book_id.clone(), row.job_id.clone());
        if !maps.contains_key(&key) {
            let snapshot = voice_session::location_snapshot(
                pool,
                cache_dir,
                row.book_id.as_deref(),
                row.job_id.as_deref(),
                row.job_resolves,
            )
            .await;
            maps.insert(key.clone(), snapshot);
        }
        if let Some(Some(snapshot)) = maps.get(&key) {
            row.location = voice_session::resolve_location(snapshot, value.max(0) as u64);
            row.chapters_touched = row.location.as_ref().map(|l| vec![l.chapter]);
        }
    }
}

async fn history_delete(
    pool: &storage::SqlitePool,
    session_id: &str,
    live_id: Option<&str>,
) -> Result<(), String> {
    if live_id == Some(session_id) {
        return Err(LIVE_ROW_ERR.to_string());
    }
    if storage::voice_journal::delete_session(pool, session_id)
        .await
        .map_err(|e| e.to_string())?
    {
        Ok(())
    } else {
        Err(NOT_FOUND_ERR.to_string())
    }
}

async fn history_forget_scope(
    pool: &storage::SqlitePool,
    scope: Option<&str>,
    live_id: Option<&str>,
) -> Result<u64, String> {
    storage::voice_journal::delete_scope(pool, scope, live_id)
        .await
        .map_err(|e| e.to_string())
}

/// `live_job_id` is the live handle's cast, compared in memory: a never-persist
/// session has no row to read, yet its cast is still live.
async fn history_job_delete_preview(
    pool: &storage::SqlitePool,
    job_id: &str,
    live_job_id: Option<&str>,
) -> Result<JobDeletePreviewDto, String> {
    let session_count = storage::voice_journal::count_for_job(pool, job_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(JobDeletePreviewDto {
        session_count,
        live: live_job_id == Some(job_id),
    })
}

// Reads (list, transcript, preview) take the live identity under the slot lock
// and release it before touching the database: `voice_session_start` holds
// the same lock across its whole dial, and a history read must not wait on a
// dial. Deletes hold the lock across the operation so the guard and the delete
// are one atom: a session cannot start on the row in between (DQA-H5).
// A zero-turn row is not history (transcript reports not found) but it is
// still deletable by id — forget-scope takes it too (DQA-H8).

#[tauri::command]
async fn voice_session_list(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    slot: tauri::State<'_, VoiceSessionSlot>,
    book_id: Option<String>,
) -> Result<Vec<SessionSummaryDto>, String> {
    let live = live_session_id(slot.0.lock().await.as_ref());
    let mut rows = history_list(&pool, book_id.as_deref(), live.as_deref()).await?;
    attach_locations(&pool, &history_cache_dir(&app)?, &mut rows).await;
    Ok(rows)
}

#[tauri::command]
async fn voice_session_transcript(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    slot: tauri::State<'_, VoiceSessionSlot>,
    session_id: String,
) -> Result<SessionDetailDto, String> {
    let live = live_session_id(slot.0.lock().await.as_ref());
    let mut detail = history_transcript(&pool, &session_id, live.as_deref()).await?;
    attach_locations(
        &pool,
        &history_cache_dir(&app)?,
        std::slice::from_mut(&mut detail.row),
    )
    .await;
    Ok(detail)
}

#[tauri::command]
async fn voice_page_peek(
    app: tauri::AppHandle,
    pool: tauri::State<'_, storage::SqlitePool>,
    book_id: String,
    job_id: Option<String>,
    page_no: i64,
) -> Result<Option<page_peek::PagePeek>, String> {
    let chapters = match job_id.as_deref() {
        Some(job_id) => {
            let cache_dir = history_cache_dir(&app)?;
            reading::session_chapters(&pool, job_id, &cache_dir).await
        }
        None => reading::book_chapters(&pool, &book_id).await,
    }
    .map_err(|e| e.to_string())?;
    let titles = storage::book::list_chapters(&pool, &book_id)
        .await
        .map(|rows| rows.into_iter().map(|row| (row.id, row.title)).collect())
        .unwrap_or_default();
    Ok(page_peek::page_peek(&chapters, &titles, page_no))
}

/// The cache the chapter maps are read from (the reading loader's own).
fn history_cache_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    storage::paths::resolve_app_cache_dir(app)
}

#[tauri::command]
async fn voice_session_delete(
    pool: tauri::State<'_, storage::SqlitePool>,
    slot: tauri::State<'_, VoiceSessionSlot>,
    session_id: String,
) -> Result<(), String> {
    slot.delete_session(&pool, &session_id).await
}

#[tauri::command]
async fn voice_session_forget_scope(
    pool: tauri::State<'_, storage::SqlitePool>,
    slot: tauri::State<'_, VoiceSessionSlot>,
    book_id: Option<String>,
) -> Result<u64, String> {
    slot.forget_scope(&pool, book_id.as_deref()).await
}

#[tauri::command]
async fn job_delete_preview(
    pool: tauri::State<'_, storage::SqlitePool>,
    slot: tauri::State<'_, VoiceSessionSlot>,
    job_id: String,
) -> Result<JobDeletePreviewDto, String> {
    let live_job = slot
        .0
        .lock()
        .await
        .as_ref()
        .filter(|handle| handle.is_alive())
        .and_then(|handle| handle.job_id().map(str::to_string));
    history_job_delete_preview(&pool, &job_id, live_job.as_deref()).await
}

#[allow(dead_code)]
struct SingleInstanceLock(Arc<std::sync::Mutex<Option<std::fs::File>>>);

impl SingleInstanceLock {
    fn new(file: std::fs::File) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(file))))
    }

    fn release(&self) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = None;
        }
    }
}

fn setup_app(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = storage::paths::resolve_app_data_dir(app.handle())?;
    let cache_dir = storage::paths::resolve_app_cache_dir(app.handle())?;
    storage::paths::ensure_private_dir(&data_dir)?;
    storage::paths::ensure_private_dir(&cache_dir)?;
    match render::ffmpeg::register_bundled(app.path().resource_dir().ok().as_deref()) {
        Some(path) => tracing::info!("ffmpeg source=bundled at={}", path.display()),
        None => tracing::info!("ffmpeg source=path"),
    }

    let lock_path = data_dir.join("txtcaster.lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    use fs4::fs_std::FileExt;
    if !lock_file.try_lock_exclusive().unwrap_or(false) {
        tracing::warn!("another instance of txtcaster is already running; exiting");
        return Err("another instance of txtcaster is already running".into());
    }
    app.manage(SingleInstanceLock::new(lock_file));

    let db_path = data_dir.join("db.sqlite");
    let pool = tauri::async_runtime::block_on(storage::open_pool(&db_path))?;
    app.manage(pool.clone());
    let key_manager = Arc::new(voice::keys::KeyManager::new(
        data_dir.join("voice_keys.vault"),
    ));
    app.manage(key_manager.clone());
    app.manage(Arc::new(ExtractionControls::default()));
    app.manage(Arc::new(ImportControls::default()));
    app.manage(Arc::new(voice_playback::PlaybackSurface::default()));
    app.manage(VoiceSessionSlot::default());
    #[cfg(feature = "book-agent")]
    {
        app.manage(agent_ipc::AgentModeTag::default());
    }
    let model_manager = Arc::new(ModelManager::new(data_dir.join("models"), pool.clone()));
    for model_id in tauri::async_runtime::block_on(model_manager.purge_retired()) {
        tracing::info!("removed retired model '{model_id}'");
    }
    app.manage(model_manager);
    let dirs = tauri::async_runtime::block_on(storage_dirs(app.handle(), &pool))?;
    {
        let exports_dir = dirs.exports.path.clone();
        let pool = pool.clone();
        tauri::async_runtime::block_on(async move {
            if let Err(err) = storage::export::refresh_sizes(&pool).await {
                tracing::warn!("export size refresh failed: {err}");
            }
            if let Err(err) = storage::job::purge_silence_fallback_cache(&pool).await {
                tracing::warn!("silence-fallback cache purge failed: {err}");
            }
            let swept = storage::export::sweep_partials(&exports_dir);
            if swept > 0 {
                tracing::info!("removed {swept} abandoned export partial file(s)");
            }
            match storage::export::adopt_orphans(&pool, &exports_dir).await {
                Ok(0) => {}
                Ok(count) => {
                    tracing::info!("adopted {count} export file(s) with no database row")
                }
                Err(err) => tracing::warn!("export adoption failed: {err}"),
            }
        });
    }
    #[cfg(debug_assertions)]
    if let Ok(seed_path) = std::env::var(reading_fixture::SEED_ENV) {
        let pool = pool.clone();
        let data_dir = data_dir.clone();
        tauri::async_runtime::block_on(async move {
            let dirs = match storage::paths::resolve(&pool, &data_dir).await {
                Ok(dirs) => dirs,
                Err(error) => {
                    tracing::warn!("reading fixture seed failed: {error}");
                    return;
                }
            };
            match reading_fixture::seed(&pool, std::path::Path::new(&seed_path), &dirs.exports.path)
                .await
            {
                Ok(path) => tracing::info!("reading fixture export at {}", path.display()),
                Err(error) => tracing::warn!("reading fixture seed failed: {error}"),
            }
        });
    }
    let finished = storage::book_files::finish_interrupted_removals(&data_dir.join("books"));
    if finished > 0 {
        tracing::info!("finished removing the files of {finished} deleted book(s)");
    }
    match voice_sample::sweep_staging(&data_dir) {
        0 => {}
        count => tracing::info!("discarded {count} unsaved voice recording(s)"),
    }
    {
        let pool = pool.clone();
        let handle = app.handle().clone();
        let cache = app.state::<ProfileCache>().inner().clone();
        tauri::async_runtime::spawn(async move {
            match cached_profile(&handle, &pool, &cache).await {
                Ok(profile) => match migrate_learned_keys(&pool, &profile).await {
                    Ok(true) => {
                        tracing::info!("re-keyed learned coefficients onto device identities")
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!("learned coefficient migration failed: {error}")
                    }
                },
                Err(error) => {
                    tracing::warn!("learned coefficient migration skipped: {error}")
                }
            }
        });
    }
    {
        let pool = pool.clone();
        let models_root = data_dir.join("models");
        tauri::async_runtime::spawn(async move {
            let installs = match storage::models::list_model_installs(&pool).await {
                Ok(installs) => installs,
                Err(error) => {
                    tracing::warn!("voice preset sync skipped: {error}");
                    return;
                }
            };
            if let Err(error) = voices::sync_preset_voice_profiles(
                &pool,
                catalog::bundled_catalog(),
                &installs,
                &models_root,
            )
            .await
            {
                tracing::warn!("voice preset sync failed: {error}");
            }
        });
    }
    let progress_app = app.handle().clone();
    let updates_app = app.handle().clone();
    let sidecar_provider: Arc<dyn SidecarProvider> = Arc::new(AppSidecarProvider {
        app: app.handle().clone(),
    });
    let mut stage_registry = StageRegistry::default();
    stage_registry.register(
        JobState::Layout,
        Arc::new(LayoutStage::new(
            sidecar_provider.clone(),
            data_dir.join("models"),
            cache_dir.clone(),
        )),
    );
    stage_registry.register(
        JobState::Vision,
        Arc::new(
            VisionStage::new(
                sidecar_provider.clone(),
                data_dir.join("models"),
                cache_dir.clone(),
            )
            .with_key_manager(key_manager.clone()),
        ),
    );
    stage_registry.register(
        JobState::Scripting,
        Arc::new(
            ScriptingStage::new(sidecar_provider.clone(), data_dir.join("models"))
                .with_key_manager(key_manager.clone()),
        ),
    );
    stage_registry.register(
        JobState::Synthesizing,
        Arc::new(
            SynthesizingStage::new(
                sidecar_provider.clone(),
                data_dir.join("models"),
                cache_dir.clone(),
            )
            .with_key_manager(key_manager.clone()),
        ),
    );
    stage_registry.register(
        JobState::Aligning,
        Arc::new({
            let engine_data_dir = data_dir.clone();
            AligningStage::new(sidecar_provider, data_dir.join("models"), cache_dir.clone())
                .with_local_models_check(move || {
                    !engine_install::capability::lite_engine_active(&engine_data_dir)
                })
        }),
    );
    stage_registry.register(JobState::Rendering, Arc::new(RenderingStage::new(&dirs)));
    let media_handle =
        tauri::async_runtime::block_on(media_server::start(sidecar::spawn::generate_token()))?;
    app.manage(media_handle);
    app.manage(lan_sender::session::LanSenderHandle::default());
    app.manage(MediaRoots(media_roots_with_cache(&dirs, &data_dir)));
    app.manage(Arc::new(voice_capture::Recorder::default()));
    let job_orchestrator = Arc::new(JobOrchestrator::start(
        pool,
        stage_registry,
        Arc::new(move |event| {
            let _ = progress_app.emit("job-progress", &event);
        }),
        Arc::new(move |update| {
            let _ = updates_app.emit("job-state", &update);
        }),
        orchestrator::DEFAULT_GRACE,
        cache_dir.clone(),
    ));
    app.manage(job_orchestrator.clone());
    tauri::async_runtime::spawn(async move {
        if let Err(error) = job_orchestrator.resume_on_launch().await {
            tracing::warn!("resume-on-launch failed: {error}");
        }
    });
    updater::commands::init(app.handle(), &data_dir);
    engine_install::commands::init(app.handle(), &data_dir);
    supervisor::start(app.handle());
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    logging::init();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(supervisor::new_handle())
        .manage(ShutdownCoordinator::default())
        .manage(window_chrome::ChromeState::default())
        .manage(DownloadRegistry::default())
        .manage(ProfileCache::default())
        .setup(|app| {
            if let Err(error) = setup_app(app) {
                tracing::error!("startup failed: {error}");
                let failure = startup::StartupFailure::classify(
                    &error.to_string(),
                    storage::paths::resolve_app_data_dir(app.handle()).ok(),
                );
                app.manage(failure);
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            startup::startup_failure,
            startup::startup_quit,
            updater::commands::update_status,
            updater::commands::update_check_now,
            updater::commands::update_install,
            updater::commands::update_cancel,
            updater::commands::update_dismiss,
            retry_failed_job,
            cast_provider_options,
            provider_fetch_models,
            provider_draft_models,
            provider_directory,
            provider_cached_catalog,
            provider_connect,
            provider_refresh_catalog,
            provider_apply_recommendations,
            provider_cancel_setup,
            estimate_cast_cost,
            cast_consent_grant,
            cast_consent_revoke,
            cast_consent_list,
            providers_overview,
            provider_slot_set_disabled,
            provider_pool_test,
            provider_pool_test_cancel,
            provider_cached_models,
            create_cloud_voice_profile,
            cast_align_advisory,
            provider_connections_list,
            provider_connection_create,
            provider_connection_update,
            provider_connection_delete_impact,
            provider_connection_delete,
            app_version,
            engine_status,
            get_catalog,
            get_hardware_profile,
            plan_import,
            plan_extraction,
            improve_extraction,
            cancel_extraction,
            active_extraction,
            active_import,
            cancel_import,
            get_compute_estimate,
            estimate_job_eta,
            render_pages,
            import_book,
            list_books,
            delete_book,
            list_exports,
            delete_export,
            delete_all_exports,
            get_chapters,
            list_voice_profiles,
            preview_voice,
            list_audio_inputs,
            start_voice_recording,
            stop_voice_recording,
            cancel_voice_recording,
            import_voice_sample,
            discard_voice_sample,
            preview_voice_sample,
            save_voice_profile,
            delete_voice_profile,
            get_media_url,
            log_ui_layout,
            log_reading_clock,
            log_page_mode,
            log_follow_state,
            log_reading_tap,
            log_ui_error,
            create_job,
            pause_job,
            resume_job,
            switch_job_to_overnight,
            cancel_job,
            delete_job,
            get_job,
            list_jobs,
            list_reviewable_segments,
            get_reading_index,
            get_reading_chapter,
            get_reading_pages,
            ensure_reading_pages,
            get_reading_figures,
            list_suppressed_blocks,
            resynthesize_segment,
            rerender_job,
            list_installed_models,
            get_default_models,
            set_default_model,
            get_onboarding_complete,
            set_onboarding_complete,
            get_theme_preference,
            set_theme_preference,
            host_locale,
            get_locale_preference,
            set_locale_preference,
            get_ingest_fallback_enabled,
            set_ingest_fallback_enabled,
            set_window_background,
            set_window_chrome,
            reveal_main_window,
            get_storage_settings,
            get_storage_breakdown,
            uninstall_engine,
            engine_install::commands::engine_install_info,
            engine_install::commands::engine_active_variant,
            engine_install::commands::install_engine,
            engine_install::commands::cancel_engine_install,
            purge_audio_cache,
            danger_self_purge,
            set_cache_cap_bytes,
            delete_model,
            download_model,
            cancel_model_download,
            list_voice_providers,
            voice_key_storage_status,
            unlock_voice_key_vault,
            use_session_only_voice_keys,
            list_voice_keys,
            probe_voice_keys,
            test_all_voice_keys,
            refresh_voice_key,
            add_voice_key,
            import_voice_keys_text,
            import_voice_keys_file,
            remove_voice_key,
            check_voice_url,
            record_voice_url_consent,
            list_cast_key_pool,
            voice_tutor_readiness,
            check_reverse_anchor_consent,
            record_reverse_anchor_consent,
            revoke_reverse_anchor_consent,
            list_reverse_anchor_consents,
            list_learning_preferences,
            forget_learning_preference,
            get_learning_privacy,
            set_personalize_learning,
            voice_retention_get,
            voice_retention_set,
            voice_playback_report,
            voice_playback_detach,
            voice_session_active,
            voice_session_start,
            #[cfg(feature = "book-agent")]
            agent_ipc::agent_session_probe,
            #[cfg(feature = "book-agent")]
            agent_ipc::agent_consent_grant,
            #[cfg(feature = "book-agent")]
            agent_ipc::agent_session_start,
            #[cfg(feature = "book-agent")]
            agent_ipc::agent_highlight_ack,
            #[cfg(feature = "book-agent")]
            agent_ipc::agent_session_stop,
            voice_session_stop,
            voice_session_send_text,
            voice_session_page_turned,
            voice_session_page_shown,
            voice_session_navigation_ack,
            voice_page_peek,
            voice_session_interrupt,
            voice_session_enter_interactive,
            voice_session_leave_interactive,
            voice_session_mute,
            voice_session_unmute,
            voice_session_gate,
            voice_session_ungate,
            voice_session_list,
            voice_session_transcript,
            voice_session_delete,
            voice_session_forget_scope,
            job_delete_preview,
            lan_send_start,
            lan_send_status,
            lan_send_stop
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    let app_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            request_shutdown(&app_handle);
        }
    });

    // The window starts hidden so the theme is applied before first paint;
    // the frontend reveals it once the stored preference has been read. If
    // the frontend never gets that far, show the window anyway rather than
    // leave the app invisible.
    let reveal_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if let Some(window) = reveal_handle.get_webview_window("main") {
            if !window.is_visible().unwrap_or(true) {
                let _ = window.show();
            }
        }
    });

    app.run(|app, event| match event {
        tauri::RunEvent::ExitRequested { api, .. } => {
            let coordinator = app.state::<ShutdownCoordinator>();
            if coordinator.finished() {
                return;
            }
            api.prevent_exit();
            if coordinator.begin() {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    shutdown_app(app).await;
                });
            }
        }
        tauri::RunEvent::WindowEvent {
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            api.prevent_close();
            request_shutdown(app);
        }
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Focused(focused),
            ..
        } => {
            window_chrome::on_focus(app, &label, focused);
        }
        _ => {}
    });
}

#[cfg(test)]
mod job_list_tests {
    use super::*;

    fn record(id: &str, scope_json: &str) -> storage::job::RenderJob {
        storage::job::RenderJob {
            id: id.into(),
            book_id: "book".into(),
            scope_json: scope_json.into(),
            mode: "narrator".into(),
            voice_profile_id: "voice".into(),
            output_format: "mp3".into(),
            model_overrides_json: None,
            resource_plan_json: None,
            vision_plan_json: None,
            tts_plan_json: None,
            align_plan_json: None,
            state: "DONE".into(),
            checkpoint_json: None,
            overnight: false,
            eta_seconds: None,
            report_json: None,
            output_paths_json: None,
            tts_seed: None,
            verify: false,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn one_unreadable_row_does_not_empty_the_job_list() {
        let pages = r#"{"kind":"pages","page_start":1,"page_end":3}"#;
        let summaries = readable_job_summaries(vec![
            record("first", pages),
            record("corrupt", "{not json"),
            record("last", pages),
        ]);
        let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["first", "last"]);
    }
}

#[cfg(test)]
mod capability_cache_tests {
    use super::*;

    #[test]
    fn only_a_known_answer_is_latched_for_the_session() {
        assert!(
            TORCH_CUDA.get().is_none(),
            "the cell starts empty so a caller can distinguish 'not asked' from 'answered no'"
        );
        let cache: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
        let unknown: Option<bool> = None;
        if let Some(answer) = unknown {
            let _ = cache.set(answer);
        }
        assert!(
            cache.get().is_none(),
            "an Unknown or timed-out probe must not latch: a single hiccup would quote cpu \
             speeds for the rest of the session on a cuda machine"
        );
        if let Some(answer) = Some(true) {
            let _ = cache.set(answer);
        }
        assert_eq!(cache.get(), Some(&true));
    }
}

#[cfg(test)]
mod learned_migration_tests {
    use super::*;

    fn card(key: &str, backend: hardware::DeviceBackend) -> hardware::DeviceInfo {
        hardware::DeviceInfo {
            key: key.into(),
            backend,
            ordinal: 0,
            vendor: "Fixture".into(),
            name: "Fixture".into(),
            memory_total_mb: Some(12_288),
            memory_free_mb: Some(12_000),
            unified: false,
            discrete: true,
            memory_source: hardware::MemorySource::Nvml,
        }
    }

    fn profile_of(devices: Vec<hardware::DeviceInfo>) -> hardware::HardwareProfile {
        hardware::HardwareProfile {
            devices,
            ram: hardware::RamInfo {
                total_mb: 65_536,
                free_mb: 48_000,
                cgroup: Default::default(),
            },
            cpu: hardware::CpuInfo {
                cores_physical: 8,
                cores_logical: 16,
                avx2: true,
                avx512: false,
            },
            disk: hardware::DiskInfo {
                models_free_gb: 100.0,
                cache_free_gb: 100.0,
            },
        }
    }

    #[test]
    fn a_non_primary_cuda_card_still_claims_the_legacy_samples() {
        let amd = hardware::DeviceInfo {
            memory_total_mb: Some(24_560),
            memory_free_mb: Some(24_000),
            ..card("pci:0000:03:00.0", hardware::DeviceBackend::Vulkan)
        };
        let profile = profile_of(vec![
            amd,
            card("pci:0000:0a:00.0", hardware::DeviceBackend::Cuda),
        ]);
        assert_ne!(
            profile.primary().map(|d| d.key.as_str()),
            Some("pci:0000:0a:00.0"),
            "the fixture only means anything while the cuda card is not the primary"
        );
        assert_eq!(
            kokoro_cuda_identity(&profile).as_deref(),
            Some("pci:0000:0a:00.0"),
            "kokoro runs on that card, so its samples are valid and must not be dropped"
        );
    }

    #[test]
    fn two_cuda_cards_cannot_attribute_a_legacy_sample() {
        let profile = profile_of(vec![
            card("pci:0000:01:00.0", hardware::DeviceBackend::Cuda),
            card("pci:0000:41:00.0", hardware::DeviceBackend::Cuda),
        ]);
        assert_eq!(kokoro_cuda_identity(&profile), None);
    }

    #[test]
    fn a_machine_with_no_cuda_card_attributes_nothing() {
        let profile = profile_of(vec![card(
            "pci:0000:03:00.0",
            hardware::DeviceBackend::Vulkan,
        )]);
        assert_eq!(kokoro_cuda_identity(&profile), None);
    }
}

#[cfg(test)]
mod media_root_tests {
    use super::*;

    async fn dirs_with_configured_exports(
        temp: &tempfile::TempDir,
    ) -> (std::path::PathBuf, storage::paths::StorageDirs) {
        let pool = storage::open_pool(&temp.path().join("db.sqlite"))
            .await
            .expect("pool");
        let data_dir = temp.path().join("appdata");
        let configured = temp.path().join("elsewhere-exports");
        std::fs::create_dir_all(&configured).expect("configured exports dir");
        storage::settings::set_setting(
            &pool,
            storage::paths::EXPORTS_DIR_KEY,
            &configured.to_string_lossy(),
        )
        .await
        .expect("set exports dir");
        let dirs = storage::paths::resolve(&pool, &data_dir)
            .await
            .expect("resolve dirs");
        (data_dir, dirs)
    }

    #[test]
    fn no_voice_recording_is_reachable_through_the_media_server() {
        let data = std::path::Path::new("/var/lib/txtcaster");
        let roots = media_roots(&storage::paths::defaults(data), data);
        for path in [
            voice_sample::reference_path(data, "profile-1"),
            voice_sample::staging_path(data, "sample-1"),
            voice_sample::voices_dir(data).join("anything.wav"),
        ] {
            assert!(
                !roots.iter().any(|root| path.starts_with(root)),
                "{} would be servable, which republishes whatever was imported",
                path.display()
            );
        }
    }

    #[test]
    fn the_audio_the_player_and_previews_need_is_still_reachable() {
        let data = std::path::Path::new("/var/lib/txtcaster");
        let roots = media_roots(&storage::paths::defaults(data), data);
        for path in [
            data.join("exports").join("book.mp3"),
            data.join("cache").join("previews").join("abc.wav"),
        ] {
            assert!(
                roots.iter().any(|root| path.starts_with(root)),
                "{} stopped being servable",
                path.display()
            );
        }
    }

    /// The writer, the shelf and the media-server allowlist must all resolve the exports
    /// directory through `storage::paths`. Before that they disagreed: the rendering stage and
    /// the purge/delete/sweep sites built `data_dir.join("exports")` by hand while the shelf read
    /// the configured directory, so with a custom exports dir every newly rendered export was
    /// invisible in the shelf, unmanageable, and unplayable.
    #[tokio::test]
    async fn a_configured_exports_dir_is_where_the_writer_the_shelf_and_the_allowlist_all_look() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (data_dir, dirs) = dirs_with_configured_exports(&temp).await;
        let configured = temp.path().join("elsewhere-exports");

        assert_eq!(
            dirs.exports_dir(),
            configured,
            "the override must survive resolution, or this test proves nothing"
        );
        assert_eq!(
            RenderingStage::new(&dirs).exports_dir(),
            configured,
            "the rendering stage must write where the shelf reads, not under the app data dir"
        );

        let roots = media_roots(&dirs, &data_dir);
        assert!(
            roots.contains(&configured),
            "the media server must serve the configured exports dir; roots: {:?}",
            roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
        );
        assert!(
            !roots.contains(&data_dir.join("exports")),
            "the media server must not keep serving a default tree the writer no longer fills"
        );
    }

    /// Pins the writer/allowlist pairing on its own, so a future edit cannot move one without
    /// the other turning this red.
    #[tokio::test]
    async fn the_media_allowlist_always_contains_the_directory_the_rendering_stage_writes_into() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (data_dir, dirs) = dirs_with_configured_exports(&temp).await;
        let written_into = RenderingStage::new(&dirs).exports_dir().to_path_buf();
        assert!(
            media_roots(&dirs, &data_dir).contains(&written_into),
            "an export the rendering stage can produce must stay playable"
        );

        let default_dirs = storage::paths::defaults(&data_dir);
        let default_written = RenderingStage::new(&default_dirs)
            .exports_dir()
            .to_path_buf();
        assert_eq!(
            default_written,
            data_dir.join("exports"),
            "with no override configured the default location is still what gets written"
        );
        assert!(
            media_roots(&default_dirs, &data_dir).contains(&default_written),
            "an export rendered into the default directory must stay playable"
        );
    }
}

#[cfg(test)]
mod catalog_view_tests {
    use super::*;
    use hardware::plan::FitBadge;
    use hardware::{CpuInfo, DiskInfo, HardwareProfile, RamInfo};

    fn profile(vram_free_mb: Option<u64>, ram_free_mb: u64) -> HardwareProfile {
        HardwareProfile {
            devices: vram_free_mb
                .map(|v| crate::hardware::test_device(Some(v)))
                .into_iter()
                .collect(),
            ram: RamInfo {
                total_mb: ram_free_mb * 2,
                free_mb: ram_free_mb,
                cgroup: Default::default(),
            },
            cpu: CpuInfo {
                cores_physical: 8,
                cores_logical: 16,
                avx2: true,
                avx512: false,
            },
            disk: DiskInfo {
                models_free_gb: 100.0,
                cache_free_gb: 100.0,
            },
        }
    }

    #[test]
    fn catalog_view_exposes_vision_badge_only_for_mmproj_models() {
        let dir = tempfile::tempdir().expect("tempdir");
        let view = build_catalog_view(
            catalog::bundled_catalog(),
            &profile(Some(4_096), 16_384),
            &[],
            dir.path(),
        );
        let gemma = view
            .models
            .iter()
            .find(|m| m.entry.id == "gemma4-e4b-q4km")
            .expect("gemma entry");
        assert!(gemma.vision_badge.is_some());
        let tts = view
            .models
            .iter()
            .find(|m| m.entry.id == "kokoro-82m")
            .expect("tts entry");
        assert!(tts.vision_badge.is_none());
    }

    #[test]
    fn view_attaches_badge_to_every_model() {
        let catalog = catalog::bundled_catalog();
        let view = build_catalog_view(
            catalog,
            &profile(Some(12_000), 16_384),
            &[],
            std::path::Path::new("/nonexistent"),
        );
        assert_eq!(view.catalog_version, catalog.catalog_version);
        assert_eq!(view.models.len(), catalog.models.len());
        for model in &view.models {
            assert!(
                model.plan.is_none(),
                "{}: no install, no plan",
                model.entry.id
            );
        }
    }

    #[test]
    fn view_no_gpu_never_yields_green_gpu_models() {
        let view = build_catalog_view(
            catalog::bundled_catalog(),
            &profile(None, 4_096),
            &[],
            std::path::Path::new("/nonexistent"),
        );
        for model in &view.models {
            if model.entry.full_gpu_vram_mb.is_some() && model.entry.min_free_ram_mb > 4_096 {
                assert_ne!(model.badge, FitBadge::Green, "{}", model.entry.id);
            }
        }
    }

    #[test]
    fn view_serializes_entry_fields_flattened() {
        let view = build_catalog_view(
            catalog::bundled_catalog(),
            &profile(Some(12_000), 16_384),
            &[],
            std::path::Path::new("/nonexistent"),
        );
        let value = serde_json::to_value(&view.models[0]).unwrap();
        assert!(value.get("id").is_some());
        assert!(value.get("badge").is_some());
        assert!(value.get("entry").is_none());
    }

    #[test]
    fn missing_gguf_file_falls_back_to_heuristic() {
        let catalog = catalog::bundled_catalog();
        let entry = catalog
            .models
            .iter()
            .find(|m| m.format == catalog::ModelFormat::Gguf)
            .expect("gguf entry");
        let installs = vec![ModelInstall {
            model_id: entry.id.clone(),
            path: format!("{}/", entry.id),
            sha256: "a".repeat(64),
            disk_bytes: entry.disk_bytes as i64,
            source: "curated".to_string(),
            installed_at: 0,
        }];
        let view = build_catalog_view(
            catalog,
            &profile(Some(12_000), 16_384),
            &installs,
            std::path::Path::new("/nonexistent"),
        );
        let model = view
            .models
            .iter()
            .find(|m| m.entry.id == entry.id)
            .expect("model in view");
        assert!(model.plan.is_none());
    }
}

#[cfg(test)]
mod cap_source_tests {
    use super::*;

    #[test]
    fn every_cap_source_reaches_the_ui_as_the_control_it_should_show() {
        assert_eq!(cap_source_name(&cache_gc::CapSource::Default), "default");
        assert_eq!(
            cap_source_name(&cache_gc::CapSource::Unlimited),
            "unlimited"
        );
        assert_eq!(
            cap_source_name(&cache_gc::CapSource::Configured(1024)),
            "custom"
        );
        assert_eq!(
            cap_source_name(&cache_gc::CapSource::Refused {
                raw: "20 GB".to_string()
            }),
            "default",
            "a stored value the reader refused is the default in force, not a chosen size"
        );
    }
}

#[cfg(test)]
mod download_event_tests {
    use super::*;

    fn shape(value: &serde_json::Value) -> Vec<String> {
        value
            .as_object()
            .expect("an event payload is an object")
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn every_download_event_names_the_model_the_same_way() {
        let progress = serde_json::to_value(model_manager::DownloadProgress {
            model_id: "kokoro-82m".into(),
            file_index: 3,
            files_total: 56,
            dest_path: "/models/tts/kokoro-82m/model.onnx".into(),
            bytes_done: 1024,
            bytes_total: 4096,
            state: model_manager::DownloadState::Downloading,
        })
        .expect("serialize progress");
        let installed = serde_json::to_value(DownloadResult::Installed {
            model_id: "kokoro-82m".into(),
        })
        .expect("serialize installed");
        let failed = serde_json::to_value(DownloadResult::Failed {
            model_id: "kokoro-82m".into(),
            error: "boom".into(),
        })
        .expect("serialize failed");

        for payload in [&progress, &installed, &failed] {
            assert_eq!(
                payload["modelId"],
                "kokoro-82m",
                "one listener keys progress, installs and failures by the same field; a payload \
                 that spells it model_id files the row under `undefined` and the queue can never \
                 clear it or name the model that failed (#107): {}",
                serde_json::to_string(payload).expect("json")
            );
            assert!(
                !shape(payload).contains(&"model_id".to_string()),
                "rename_all on an enum renames its variants, not its fields, so this asserts the \
                 fields were renamed too: {}",
                serde_json::to_string(payload).expect("json")
            );
        }

        assert_eq!(installed["state"], "installed");
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error"], "boom");
        assert_eq!(progress["fileIndex"], 3);
    }

    #[test]
    fn the_e2e_mock_emits_the_download_events_this_side_serializes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../e2e/tauriMock.ts");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("mock source {}: {error}", path.display()));
        assert!(
            source.contains("model-download-finished"),
            "the mock is the only listener the e2e suite ever talks to"
        );
        assert!(
            !source.contains("model_id"),
            "the mock spelled this event correctly while the app spelled it model_id, which is \
             why an e2e that asserts `Installed ✓` passed against a backend whose payload the \
             wizard could not read (#107). Both ends are pinned here so the mock cannot be the \
             more correct of the two again: {path:?}"
        );
    }
}
#[cfg(test)]
mod gate_command_seam_tests {
    use super::*;

    /// Kritias round 3 finding 2: the tauri boundary must not lie. No
    /// session = already satisfied (Ok); a LIVE session whose mailbox
    /// refuses delivery = Err the caller can surface. The old code
    /// returned Ok on a refused send — the frontend then believed a mute
    /// the actor never received.
    #[tokio::test]
    async fn gate_commands_propagate_mailbox_failure_and_keep_the_no_session_contract() {
        // No session: the request is already satisfied.
        assert!(
            deliver_gate_command(None, voice_session::SessionCommand::Mute { reason: None })
                .is_ok()
        );
        // Live mailbox: delivered.
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let handle = voice_session::SessionHandle::for_tests(tx);
        assert!(deliver_gate_command(
            Some(&handle),
            voice_session::SessionCommand::GateCapture { reason: None }
        )
        .is_ok());
        // Full mailbox: reports session_busy instead of no session.
        let (busy_tx, _busy_rx) = tokio::sync::mpsc::channel(1);
        let busy_handle = voice_session::SessionHandle::for_tests(busy_tx);
        assert!(deliver_gate_command(
            Some(&busy_handle),
            voice_session::SessionCommand::GateCapture { reason: None }
        )
        .is_ok());
        assert_eq!(
            deliver_gate_command(
                Some(&busy_handle),
                voice_session::SessionCommand::GateCapture { reason: None }
            ),
            Err("session_busy".to_string())
        );

        assert!(rx.recv().await.is_some());
        // Dead mailbox (actor gone): the caller must SEE the failure.
        drop(rx);
        for command in [
            voice_session::SessionCommand::Mute { reason: None },
            voice_session::SessionCommand::Unmute { reason: None },
            voice_session::SessionCommand::GateCapture { reason: None },
            voice_session::SessionCommand::UngateCapture { reason: None },
        ] {
            assert!(
                deliver_gate_command(Some(&handle), command).is_err(),
                "a refused delivery must never report success"
            );
        }
    }
}

/// F11-U1 command-seam pins (§7.1 A4, §7.4 D4–D7, §7.5 DQA-H5/H7, the
/// `job_delete_preview` contract). They drive the pool-level seams the
/// `voice_session_*` history commands are thin wrappers over, with the live
/// row identity passed explicitly.
#[cfg(test)]
mod voice_history_commands {
    use super::*;
    use storage::voice_journal::fixture::*;

    fn ids(rows: &[SessionSummaryDto]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    /// A4: an `ended_at NULL` row that is not the live slot's lists as ended
    /// with reason "unexpected" and `ended_at` = its last turn's `created_at`.
    #[tokio::test]
    async fn a4_orphan_live_rows_list_as_ended_unexpectedly_with_the_last_turn_as_ended_at() {
        let (_dir, pool) = mixed_pool().await;
        let rows = history_list(&pool, Some(BOOK), None).await.unwrap();
        let orphan = rows
            .iter()
            .find(|r| r.id == S_ORPHAN)
            .expect("H4 row listed");
        assert_eq!(orphan.end_reason, voice_session::EndReason::Unexpected);
        assert_eq!(orphan.ended_at, 5100);
        assert_eq!(orphan.duration_ms, 100);
        let a = rows.iter().find(|r| r.id == S_A).unwrap();
        assert_eq!(a.end_reason, voice_session::EndReason::Ended);
        assert_eq!(a.ended_at, 1500);
        assert_eq!(a.duration_ms, 500);
        assert_eq!(a.turn_count, 2);
        // The derive rule never writes: the stored row is untouched.
        let stored = storage::voice_journal::get_session(&pool, S_ORPHAN)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.ended_at, None);
    }

    /// DQA-H7 (D-9 (a): no sweep): while that row is live it is not listed at
    /// all, is never reported as ended, and keeps `ended_at NULL`.
    #[tokio::test]
    async fn dqa_h7_the_live_row_is_not_listed_and_keeps_ended_at_null() {
        let (_dir, pool) = mixed_pool().await;
        let rows = history_list(&pool, Some(BOOK), Some(S_ORPHAN))
            .await
            .unwrap();
        assert_eq!(ids(&rows), vec![S_DIRECT, S_B, S_A]);
        assert!(rows
            .iter()
            .all(|r| r.end_reason == voice_session::EndReason::Ended));
        let stored = storage::voice_journal::get_session(&pool, S_ORPHAN)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.ended_at, None);
    }

    /// D4 (refusal half) + DQA-H5: deleting the live row is refused and the
    /// row and its transcript are untouched, so the actor's next write lands
    /// on an existing row. The lock atomicity of the command wrapper is not
    /// exercised here (it needs a tauri runtime); the seam is what is pinned.
    #[tokio::test]
    async fn d4_deleting_the_live_row_is_refused_and_dqa_h5_the_row_stays_intact_and_writable() {
        let (_dir, pool) = mixed_pool().await;
        let err = history_delete(&pool, S_ORPHAN, Some(S_ORPHAN))
            .await
            .expect_err("live row delete must be refused");
        assert!(err.contains("live"), "{err}");
        assert!(storage::voice_journal::get_session(&pool, S_ORPHAN)
            .await
            .unwrap()
            .is_some());
        storage::voice_journal::append_turn(
            &pool,
            &storage::voice_journal::TranscriptTurn {
                session_id: S_ORPHAN.to_string(),
                turn_idx: 1,
                role: "assistant".into(),
                text: "still journaling".into(),
                created_at: 5200,
                summarized: false,
            },
        )
        .await
        .expect("the actor's next write succeeds after the refusal");
        assert_eq!(transcript_count(&pool, S_ORPHAN).await, 2);
        // A non-live row deletes while another is live.
        history_delete(&pool, S_A, Some(S_ORPHAN)).await.unwrap();
        assert!(storage::voice_journal::get_session(&pool, S_A)
            .await
            .unwrap()
            .is_none());
        assert!(history_delete(&pool, "no-such-row", None).await.is_err());
    }

    /// D5 at the command seam: forget-scope skips the live row.
    #[tokio::test]
    async fn d5_forget_scope_keeps_the_live_row() {
        let (_dir, pool) = mixed_pool().await;
        let deleted = history_forget_scope(&pool, Some(BOOK), Some(S_ORPHAN))
            .await
            .unwrap();
        assert_eq!(deleted, 4);
        assert_eq!(
            session_ids(&pool).await,
            vec![S_ORPHAN, S_SOLO, S_SOLO_EMPTY]
        );
    }

    /// `job_delete_preview` [R2]: count = what cascades (D12), `live` = the
    /// live handle's cast is this one (in memory, so a never-persist session
    /// counts as live too).
    #[tokio::test]
    async fn job_delete_preview_reports_the_cascade_count_and_the_live_flag() {
        let (_dir, pool) = mixed_pool().await;
        let idle = history_job_delete_preview(&pool, JOB_A, None)
            .await
            .unwrap();
        assert_eq!((idle.session_count, idle.live), (3, false));
        let live = history_job_delete_preview(&pool, JOB_A, Some(JOB_A))
            .await
            .unwrap();
        assert_eq!((live.session_count, live.live), (3, true));
        let other = history_job_delete_preview(&pool, JOB_B, Some(JOB_A))
            .await
            .unwrap();
        assert_eq!((other.session_count, other.live), (1, false));
        let none = history_job_delete_preview(&pool, "no-such-job", None)
            .await
            .unwrap();
        assert_eq!((none.session_count, none.live), (0, false));
    }

    #[tokio::test]
    async fn transcript_returns_the_summary_row_and_every_turn_in_turn_idx_order() {
        let (_dir, pool) = mixed_pool().await;
        let detail = history_transcript(&pool, S_A, None).await.unwrap();
        assert_eq!(detail.row.id, S_A);
        assert_eq!(
            detail.turns.iter().map(|t| t.turn_idx).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(detail.turns[0].role, "user");
        assert_eq!(detail.turns[1].text, "s-a turn 1");
        assert!(history_transcript(&pool, "no-such-row", None)
            .await
            .is_err());
        assert!(
            history_transcript(&pool, S_EMPTY, None).await.is_err(),
            "an H3 row is not history"
        );
    }

    #[tokio::test]
    async fn history_carries_a_stored_voiceprint_and_drops_a_malformed_one() {
        let (_dir, pool) = mixed_pool().await;
        let bars: Vec<u8> = (0..crate::voice::mic::VOICEPRINT_BARS as u8)
            .map(|bar| bar * 12)
            .collect();
        storage::voice_journal::set_turn_voiceprint(&pool, S_A, 0, &bars)
            .await
            .unwrap();
        let detail = history_transcript(&pool, S_A, None).await.unwrap();
        assert_eq!(detail.turns[0].voiceprint.as_deref(), Some(bars.as_slice()));
        assert_eq!(detail.turns[1].voiceprint, None);
        let wire = serde_json::to_value(&detail.turns[1]).unwrap();
        assert!(wire.get("voiceprint").is_none());

        sqlx::query(
            "UPDATE voice_transcript SET voiceprint = ? WHERE session_id = ? AND turn_idx = 0",
        )
        .bind("zz".repeat(crate::voice::mic::VOICEPRINT_BARS))
        .bind(S_A)
        .execute(&pool)
        .await
        .unwrap();
        let detail = history_transcript(&pool, S_A, None).await.unwrap();
        assert_eq!(detail.turns[0].voiceprint, None);
    }

    #[test]
    fn summary_excerpt_is_the_first_sentence_clipped_at_160_on_a_word_boundary() {
        assert_eq!(
            summary_excerpt("Inertia was defined. Then momentum."),
            "Inertia was defined."
        );
        assert_eq!(summary_excerpt("No terminator here"), "No terminator here");
        let long = format!("{} tail.", "word ".repeat(40).trim_end());
        let excerpt = summary_excerpt(&long);
        assert!(
            excerpt.chars().count() <= 160,
            "{}",
            excerpt.chars().count()
        );
        assert!(!excerpt.ends_with(' '));
        assert!(long.starts_with(excerpt.trim_end_matches('…')));
        assert!(
            excerpt.ends_with("word…"),
            "clipped on a word boundary: {excerpt}"
        );
        assert_eq!(summary_excerpt("   "), "");
    }

    #[tokio::test]
    async fn position_kind_and_provider_flag_follow_the_session_shape() {
        let (_dir, pool) = mixed_pool().await;
        let book = history_list(&pool, Some(BOOK), None).await.unwrap();
        let a = book.iter().find(|r| r.id == S_A).unwrap();
        assert_eq!(
            (a.position.kind.as_str(), a.position.value),
            ("ms", Some(10_000))
        );
        assert!(a.job_resolves);
        assert_eq!(a.summary_excerpt.as_deref(), Some("Inertia was defined."));
        assert!(
            a.provider_changed,
            "fixture provider differs from the current channel"
        );
        assert_eq!(a.chapters_touched, None);
        let direct = book.iter().find(|r| r.id == S_DIRECT).unwrap();
        assert_eq!(
            (direct.position.kind.as_str(), direct.position.value),
            ("offset", Some(30_000))
        );
        assert!(!direct.job_resolves);
        assert_eq!(direct.summary_excerpt, None);
        let solo = history_list(&pool, None, None).await.unwrap();
        assert_eq!(
            (solo[0].position.kind.as_str(), solo[0].position.value),
            ("none", None)
        );
        storage::voice_journal::insert_session(
            &pool,
            &storage::voice_journal::VoiceSessionRow {
                id: "s-current".into(),
                book_id: None,
                job_id: None,
                provider_base_url: voice_session::GEMINI_KEY_BASE_URL.to_string(),
                started_at: 8000,
                ended_at: Some(8100),
                playback_pos_ms: 0,
                summary_text: None,
                summary_turns: 0,
                end_reason: "ended".to_string(),
            },
        )
        .await
        .unwrap();
        storage::voice_journal::append_turn(
            &pool,
            &storage::voice_journal::TranscriptTurn {
                session_id: "s-current".into(),
                turn_idx: 0,
                role: "user".into(),
                text: "hi".into(),
                created_at: 8050,
                summarized: false,
            },
        )
        .await
        .unwrap();
        let solo = history_list(&pool, None, None).await.unwrap();
        assert!(
            !solo
                .iter()
                .find(|r| r.id == "s-current")
                .unwrap()
                .provider_changed
        );
    }

    /// The live-row identity seam: only an alive handle names a live row.
    #[tokio::test]
    async fn live_session_id_is_reported_only_for_an_alive_handle() {
        assert_eq!(live_session_id(None), None);
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let handle = voice_session::SessionHandle::for_tests_with_id(tx, "s-live", Some("job-x"));
        assert_eq!(live_session_id(Some(&handle)).as_deref(), Some("s-live"));
        assert_eq!(handle.job_id(), Some("job-x"));
        drop(rx);
        assert_eq!(
            live_session_id(Some(&handle)),
            None,
            "a dead actor holds no row"
        );
    }

    /// D6′: no code path deletes a `voice_session` row except the two store
    /// functions (and the FK cascades). Allowlist: the store module, and the
    /// one pre-existing test-only wipe in the runtime twin's pin.
    #[test]
    fn d6_no_code_path_deletes_voice_session_rows_outside_the_store() {
        let needle = concat!("DELETE FROM ", "voice_session");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits: Vec<(String, usize)> = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let n = text.matches(needle).count();
                    if n > 0 {
                        let rel = path
                            .strip_prefix(&root)
                            .unwrap()
                            .components()
                            .map(|part| part.as_os_str().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join("/");
                        hits.push((rel, n));
                    }
                }
            }
        }
        hits.sort();
        assert_eq!(
            hits,
            vec![
                ("storage/voice_journal.rs".to_string(), 2),
                ("voice_journal.rs".to_string(), 1),
            ],
            "voice_session deletes live only in delete_session/delete_scope (+ the twin pin's test wipe)"
        );
    }

    /// D7 is structural, not behavioural: every `history_*` seam takes only
    /// `&SqlitePool` and ids — no channel type is reachable from this path, so
    /// nothing can be emitted to a provider. A MockChannel op-log assertion
    /// would be tautological here and is deliberately absent.
    #[test]
    fn d7_history_seams_take_only_the_pool() {
        fn pool_only<F>(_: F)
        where
            F: for<'a> Fn(
                &'a storage::SqlitePool,
                Option<&'a str>,
                Option<&'a str>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<Vec<SessionSummaryDto>, String>> + 'a>,
            >,
        {
        }
        pool_only(|pool, scope, live| Box::pin(history_list(pool, scope, live)));
    }

    /// C1: the end reason is a typed enum with one wire string per variant —
    /// the TS table maps every one of them; the history derive yields the two
    /// a stored row can prove.
    #[test]
    fn resume_c1_end_reason_is_typed_and_every_variant_has_a_wire_string() {
        use voice_session::EndReason;
        let all = EndReason::ALL;
        let variant_count = |r: EndReason| match r {
            EndReason::Ended
            | EndReason::Unexpected
            | EndReason::Expired
            | EndReason::ConnectionClosed => 4usize,
        };
        assert_eq!(all.len(), variant_count(EndReason::Ended));
        for (i, r) in all.iter().enumerate() {
            assert_eq!(r.index(), i);
        }
        let ts = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/sessionHistoryStore.ts"
        ))
        .unwrap();
        let ts_list: Vec<&str> = ts
            .split("export const END_REASONS = [")
            .nth(1)
            .unwrap()
            .split("] as const;")
            .next()
            .unwrap()
            .split('"')
            .skip(1)
            .step_by(2)
            .collect();
        let wire: Vec<String> = all
            .iter()
            .map(|r| {
                serde_json::to_value(r)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            wire,
            ["ended", "unexpected", "expired", "connection_closed"]
        );
        assert_eq!(ts_list, wire);
        for (reason, s) in all.iter().zip(&wire) {
            assert_eq!(reason.as_str(), s);
        }
        let ts = include_str!("../../src/sessionHistoryStore.ts");
        for s in &wire {
            assert!(ts.contains(&format!("\"{s}\"")), "TS side lacks {s}");
        }
        let migration = include_str!("../migrations/0025_voice_session_end_reason.sql");
        let check = migration
            .split("CHECK (end_reason IN (")
            .nth(1)
            .and_then(|rest| rest.split("))").next())
            .expect("the migration pins the CHECK list");
        let checked: Vec<String> = check
            .split(',')
            .map(|s| s.trim().trim_matches('\'').to_string())
            .collect();
        assert_eq!(
            checked, wire,
            "migration CHECK values equal EndReason::ALL wire strings"
        );
        for r in all {
            assert_eq!(voice_session::EndReason::parse(r.as_str()), Some(r));
        }
        assert_eq!(voice_session::EndReason::parse("stopped"), None);
    }

    /// Ruling #32 (b): a stored row keeps the typed reason the runtime wrote;
    /// the read path returns it so S6/S7 render for history, and a row that
    /// never ended still derives `Unexpected`.
    #[tokio::test]
    async fn f11_u4_stored_end_reason_survives_the_read_path() {
        let (_dir, pool) = mixed_pool().await;
        storage::voice_journal::end_session(&pool, S_A, 9_999, "expired")
            .await
            .unwrap();
        let detail = history_transcript(&pool, S_A, None).await.unwrap();
        assert_eq!(detail.row.end_reason, voice_session::EndReason::Expired);
        assert_eq!(detail.row.ended_at, 9_999);
        let rows = history_list(&pool, Some(BOOK), None).await.unwrap();
        let a = rows.iter().find(|r| r.id == S_A).unwrap();
        assert_eq!(a.end_reason, voice_session::EndReason::Expired);
        let orphan = rows
            .iter()
            .find(|r| r.end_reason == voice_session::EndReason::Unexpected);
        assert!(orphan.is_some(), "a NULL ended_at still derives Unexpected");
    }

    /// C1/C2: the derive still reports the provable pair; the detail carries
    /// the whole summary paragraph beside the excerpt.
    #[tokio::test]
    async fn resume_c2_detail_carries_the_summary_paragraph() {
        let (_dir, pool) = mixed_pool().await;
        let detail = history_transcript(&pool, S_A, None).await.unwrap();
        assert_eq!(
            detail.summary_text.as_deref(),
            Some("Inertia was defined. Then momentum.")
        );
        assert_eq!(detail.row.end_reason, voice_session::EndReason::Ended);
        let orphan = history_transcript(&pool, S_ORPHAN, None).await.unwrap();
        assert_eq!(orphan.summary_text, None);
        assert_eq!(orphan.row.end_reason, voice_session::EndReason::Unexpected);
    }

    /// C3: rows carry the §5.3.1 location when their map resolves; H9 (job of
    /// another book) and H5 never do; an unreadable map is `None`, not a guess.
    #[tokio::test]
    async fn resume_c3_rows_carry_a_location_only_when_the_map_resolves() {
        let (dir, pool) = mixed_pool().await;
        let mut rows = history_list(&pool, Some(BOOK), None).await.unwrap();
        attach_locations(&pool, dir.path(), &mut rows).await;
        assert!(
            rows.iter()
                .all(|r| r.location.is_none() && r.chapters_touched.is_none()),
            "no layout in this cache: nothing resolves, nothing is invented"
        );
        let mut solo = history_list(&pool, None, None).await.unwrap();
        attach_locations(&pool, dir.path(), &mut solo).await;
        assert!(solo.iter().all(|r| r.location.is_none()));
    }

    /// C4 (DQA-H5 at the command boundary): the slot method refuses the live
    /// row, and holds the slot lock for the WHOLE delete — a start cannot
    /// slip in between the guard and the delete.
    #[tokio::test]
    async fn resume_c4_slot_delete_refuses_the_live_row_and_holds_the_lock_throughout() {
        let (_dir, pool) = mixed_pool().await;
        let slot = Arc::new(VoiceSessionSlot::default());
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        *slot.0.lock().await = Some(voice_session::SessionHandle::for_tests_with_id(
            tx, S_A, None,
        ));
        assert_eq!(
            slot.delete_session(&pool, S_A).await,
            Err(LIVE_ROW_ERR.to_string())
        );
        assert!(fixture_row_exists(&pool, S_A).await);

        let mut blocker = pool.acquire().await.unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let task = tokio::spawn({
            let slot = Arc::clone(&slot);
            let pool = pool.clone();
            async move { slot.delete_session(&pool, S_B).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(
            slot.0.try_lock().is_err(),
            "the slot lock must be held while the delete is in flight"
        );
        sqlx::query("COMMIT").execute(&mut *blocker).await.unwrap();
        drop(blocker);
        task.await.unwrap().unwrap();
        assert!(!fixture_row_exists(&pool, S_B).await);
        assert!(slot.0.try_lock().is_ok(), "released after the delete");
    }

    async fn fixture_row_exists(pool: &storage::SqlitePool, id: &str) -> bool {
        session_ids(pool).await.iter().any(|s| s == id)
    }
}

/// F11-U6 pins (spec §7.4 D1, D2, D11; §6.3 J7.4): the retention commands'
/// seams and the guarded cast delete, against the mixed history fixture
/// (`job-a` carries S_A, S_EMPTY, S_ORPHAN; `job-b` carries S_B; S_DIRECT is
/// BookDirect; S_SOLO/S_SOLO_EMPTY are standalone).
#[cfg(test)]
mod f11_u6_retention_and_delete_gate {
    use super::*;
    use storage::voice_journal::fixture::*;

    async fn session_ids(pool: &storage::SqlitePool) -> Vec<String> {
        sqlx::query_scalar::<_, String>("SELECT id FROM voice_session ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn stored_retention(pool: &storage::SqlitePool) -> Option<String> {
        storage::settings::get_setting(pool, voice_journal::TRANSCRIPT_RETENTION_KEY)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn retention_absent_reads_keep_and_writes_the_explicit_default_once() {
        let (_dir, pool) = mixed_pool().await;
        assert_eq!(stored_retention(&pool).await, None);
        assert_eq!(retention_read(&pool).await.unwrap(), "keep");
        assert_eq!(stored_retention(&pool).await.as_deref(), Some("keep"));
        assert!(matches!(
            voice_journal::JournalMode::from_settings(&pool).await,
            voice_journal::JournalMode::Persisted(_)
        ));
    }

    /// D1: choosing Never is what the next session's journal reads.
    #[tokio::test]
    async fn retention_set_never_reads_never_and_selects_the_ephemeral_journal() {
        let (_dir, pool) = mixed_pool().await;
        assert_eq!(retention_write(&pool, "never").await.unwrap(), "never");
        assert_eq!(retention_read(&pool).await.unwrap(), "never");
        assert!(matches!(
            voice_journal::JournalMode::from_settings(&pool).await,
            voice_journal::JournalMode::Ephemeral
        ));
        assert_eq!(retention_write(&pool, "keep").await.unwrap(), "keep");
        assert!(matches!(
            voice_journal::JournalMode::from_settings(&pool).await,
            voice_journal::JournalMode::Persisted(_)
        ));
    }

    /// D2: switching to Never deletes nothing (H13).
    #[tokio::test]
    async fn retention_switch_to_never_deletes_no_rows() {
        let (_dir, pool) = mixed_pool().await;
        let before = session_ids(&pool).await;
        assert_eq!(before.len(), 7);
        retention_write(&pool, "never").await.unwrap();
        assert_eq!(session_ids(&pool).await, before);
    }

    #[tokio::test]
    async fn retention_rejects_unknown_values_and_leaves_the_stored_one() {
        let (_dir, pool) = mixed_pool().await;
        retention_write(&pool, "never").await.unwrap();
        assert!(retention_write(&pool, "30d").await.is_err());
        assert!(retention_write(&pool, "").await.is_err());
        assert_eq!(stored_retention(&pool).await.as_deref(), Some("never"));
        // An unknown stored value reads as keep (persisted) and is left alone.
        storage::settings::set_setting(&pool, voice_journal::TRANSCRIPT_RETENTION_KEY, "30d")
            .await
            .unwrap();
        assert_eq!(retention_read(&pool).await.unwrap(), "keep");
        assert_eq!(stored_retention(&pool).await.as_deref(), Some("30d"));
    }

    /// J7.4 / §6.3: a cast with a live tutor session cannot be deleted, and
    /// the refusal deletes nothing — not the job row, not one session row.
    #[tokio::test]
    async fn delete_job_is_refused_while_the_cast_has_a_live_session_and_deletes_nothing() {
        let (_dir, pool) = mixed_pool().await;
        let before = session_ids(&pool).await;
        let error = delete_job_guarded(&pool, JOB_A, Some(JOB_A))
            .await
            .expect_err("live cast delete must be refused");
        assert!(error.contains("live tutor session"), "{error}");
        assert!(storage::job::get_job(&pool, JOB_A).await.unwrap().is_some());
        assert_eq!(session_ids(&pool).await, before);
    }

    /// R2 + D11: without a live session the delete runs and the cascade
    /// takes exactly that cast's rows — zero-turn and orphan-live rows
    /// included — while the other cast's, BookDirect and standalone rows
    /// survive.
    #[tokio::test]
    async fn delete_job_cascades_only_that_casts_rows() {
        let (_dir, pool) = mixed_pool().await;
        delete_job_guarded(&pool, JOB_A, None).await.unwrap();
        assert!(storage::job::get_job(&pool, JOB_A).await.unwrap().is_none());
        assert_eq!(
            session_ids(&pool).await,
            vec![S_B, S_DIRECT, S_SOLO, S_SOLO_EMPTY]
        );
    }

    /// The guard is per cast: a live session on another cast does not block.
    #[tokio::test]
    async fn delete_job_with_the_live_session_on_another_cast_is_allowed() {
        let (_dir, pool) = mixed_pool().await;
        delete_job_guarded(&pool, JOB_B, Some(JOB_A)).await.unwrap();
        assert!(storage::job::get_job(&pool, JOB_B).await.unwrap().is_none());
        assert!(storage::job::get_job(&pool, JOB_A).await.unwrap().is_some());
        let ids = session_ids(&pool).await;
        assert!(!ids.iter().any(|id| id == S_B));
        assert!(ids.iter().any(|id| id == S_A));
    }

    /// The settled-state rule still holds under the guard's `None`.
    #[tokio::test]
    async fn delete_job_still_refuses_an_unsettled_cast() {
        let (_dir, pool) = mixed_pool().await;
        sqlx::query("UPDATE render_job SET state = 'SYNTHESIZING' WHERE id = ?")
            .bind(JOB_A)
            .execute(&pool)
            .await
            .unwrap();
        let error = delete_job_guarded(&pool, JOB_A, None).await.unwrap_err();
        assert!(error.contains("SYNTHESIZING"), "{error}");
        assert!(storage::job::get_job(&pool, JOB_A).await.unwrap().is_some());
    }
}

#[cfg(test)]
mod import_controls_tests {
    use super::*;

    #[test]
    fn import_controls_lifecycle_and_cancellation() {
        let controls = ImportControls::default();
        assert!(controls.active().is_none());

        let (handle, receiver) = orchestrator::stage::control_channel();
        controls.register("imp-1", "test.pdf", handle);

        let active = controls.active().expect("active import");
        assert_eq!(active.import_id, "imp-1");
        assert_eq!(active.filename, "test.pdf");
        assert_eq!(active.page, 0);
        assert_eq!(active.total, 0);
        assert_eq!(active.stage, "extract");

        controls.note_progress("imp-1", 5, 20, "ocr");
        let active = controls.active().expect("active import");
        assert_eq!(active.page, 5);
        assert_eq!(active.total, 20);
        assert_eq!(active.stage, "ocr");

        assert_eq!(receiver.current(), orchestrator::stage::Control::Run);
        assert!(controls.cancel());
        assert_eq!(receiver.current(), orchestrator::stage::Control::Cancel);

        controls.release("imp-1");
        assert!(controls.active().is_none());
    }

    #[test]
    fn import_scope_guard_releases_on_drop() {
        let controls = Arc::new(ImportControls::default());
        let (handle, _receiver) = orchestrator::stage::control_channel();
        controls.register("imp-guard", "book.pdf", handle);

        {
            let _guard = ImportScopeGuard {
                controls: Arc::clone(&controls),
                import_id: "imp-guard".to_string(),
            };
            assert!(controls.active().is_some());
        }

        assert!(controls.active().is_none());
    }

    #[test]
    fn import_controls_rejects_progress_for_different_import_id() {
        let controls = ImportControls::default();
        let (handle, _receiver) = orchestrator::stage::control_channel();
        controls.register("imp-a", "book.pdf", handle);

        controls.note_progress("imp-b", 10, 50, "layout");
        let active = controls.active().expect("active import");
        assert_eq!(active.page, 0);
        assert_eq!(active.stage, "extract");
    }
}

#[cfg(test)]
mod engine_uninstall_tests {
    use super::remove_engine_dir;

    #[tokio::test]
    async fn removing_the_engine_dir_succeeds_when_present_and_when_already_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = dir.path().join("engine");
        std::fs::create_dir_all(&engine).expect("engine dir");
        std::fs::write(engine.join("sidecar.bin"), b"binary").expect("engine file");
        remove_engine_dir(engine.clone())
            .await
            .expect("present dir removes");
        assert!(!engine.exists());
        remove_engine_dir(engine)
            .await
            .expect("absent dir is not an error");
    }
}

#[cfg(test)]
mod audios_shelf_delete_tests {
    use super::delete_export_and_row;
    use crate::storage;

    const CAST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct Shelf {
        _dir: tempfile::TempDir,
        pool: storage::SqlitePool,
        exports: std::path::PathBuf,
        cast: std::path::PathBuf,
        output: std::path::PathBuf,
        partial: std::path::PathBuf,
    }

    async fn shelf_with_a_job_in(state: &str) -> Shelf {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = storage::open_pool(&dir.path().join("test.db"))
            .await
            .expect("pool");
        let exports = dir.path().join("exports");
        std::fs::create_dir_all(&exports).expect("exports dir");
        let cast = exports.join(CAST_A);
        std::fs::create_dir_all(&cast).expect("cast dir");
        let output = cast.join("My Book - Ch 1.mp3");
        std::fs::write(&output, b"the good export").expect("published export");
        let partial = storage::export::partial_sibling(&output);
        std::fs::write(&partial, b"half an encode").expect("partial");
        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1);",
        )
        .execute(&pool)
        .await
        .expect("book and voice profile");
        sqlx::query(
            "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                      output_format, state, output_paths_json, created_at, \
                                      updated_at) \
             VALUES ('j1', 'b1', '{}', 'narrator', 'vp1', 'mp3', ?, ?, 1, 1)",
        )
        .bind(state)
        .bind(serde_json::to_string(&[output.to_string_lossy().as_ref()]).expect("paths"))
        .execute(&pool)
        .await
        .expect("render job");
        sqlx::query(
            "INSERT INTO export (id, book_id, job_id, path, bytes, format, mode, created_at) \
             VALUES ('e1', 'b1', 'j1', ?, 15, 'mp3', 'narrator', 1)",
        )
        .bind(output.to_string_lossy().as_ref())
        .execute(&pool)
        .await
        .expect("export row");

        Shelf {
            _dir: dir,
            pool,
            exports,
            cast,
            output,
            partial,
        }
    }

    #[tokio::test]
    async fn the_shelf_delete_button_keeps_the_partial_a_live_encode_is_writing() {
        let shelf = shelf_with_a_job_in("RENDERING").await;

        delete_export_and_row(&shelf.pool, &shelf.exports, "e1")
            .await
            .expect("the export the user deleted is still deleted");

        assert!(
            !shelf.output.exists(),
            "the published export is what the delete button asks for, and it goes"
        );
        assert!(
            shelf.partial.exists(),
            "this command takes no mutation gate, so it runs beside a live render, and the \
             sibling is the path ffmpeg has open: unlinking it here leaves the encode writing to \
             an orphan and fails its publish, which is the defect the guarded variant exists to \
             close"
        );
        assert!(shelf.cast.exists());
        assert_eq!(
            storage::export::list_exports(&shelf.pool)
                .await
                .expect("rows")
                .len(),
            0,
            "the row goes with the file it described, so the shelf does not offer the cast again"
        );
    }

    #[tokio::test]
    async fn the_shelf_delete_button_clears_the_partial_and_prunes_once_the_job_finished() {
        let shelf = shelf_with_a_job_in("DONE").await;

        delete_export_and_row(&shelf.pool, &shelf.exports, "e1")
            .await
            .expect("delete");

        assert!(!shelf.output.exists());
        assert!(
            !shelf.partial.exists(),
            "a finished job owns nothing, so the guard must not become a blanket refusal that \
             leaves the cast directory occupied and unaccounted for"
        );
        assert!(
            !shelf.cast.exists(),
            "a real cast directory is still pruned the moment it is emptied"
        );
        assert!(shelf.exports.exists());
    }

    #[tokio::test]
    async fn the_shelf_delete_button_reports_an_unknown_id_without_touching_the_files() {
        let shelf = shelf_with_a_job_in("RENDERING").await;

        let error = delete_export_and_row(&shelf.pool, &shelf.exports, "missing")
            .await
            .expect_err("an unknown id is refused");

        assert_eq!(error, "export 'missing' not found");
        assert!(shelf.output.exists() && shelf.partial.exists());
        assert!(shelf.cast.exists());
    }
}

#[cfg(test)]
mod provider_setup_query_tests {
    use super::*;

    #[tokio::test]
    async fn opening_cast_options_never_rewrites_models_or_provisions_voices() {
        let dir = tempfile::tempdir().unwrap();
        let pool = storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .unwrap();
        let record = providers::ConnectionStore::create(
            &pool,
            "My older choice",
            "gemini",
            None,
            false,
            "gemini-2.5-flash",
            1,
        )
        .await
        .unwrap();
        let before = storage::settings::list_settings_with_prefix(&pool, "provider_")
            .await
            .unwrap();
        let app = tauri::test::mock_app();
        app.manage(pool.clone());
        for _ in 0..2 {
            let options = cast_provider_options(app.state()).await.unwrap();
            assert_eq!(options.script_connections.len(), 1);
            assert!(options.tts_voices.is_empty());
        }
        assert_eq!(
            storage::settings::list_settings_with_prefix(&pool, "provider_")
                .await
                .unwrap(),
            before
        );
        assert_eq!(
            providers::ConnectionStore::get(&pool, &record.id)
                .await
                .unwrap(),
            record
        );
        assert!(storage::voice::list_voice_profiles(&pool)
            .await
            .unwrap()
            .is_empty());
    }
}

#[cfg(test)]
mod model_removal_tests {
    use super::*;

    #[tokio::test]
    async fn retrying_removal_finishes_default_cleanup_after_a_partial_failure() {
        let dir = tempfile::tempdir().unwrap();
        let pool = storage::open_pool(&dir.path().join("models.sqlite"))
            .await
            .unwrap();
        let root = dir.path().join("models");
        std::fs::create_dir_all(root.join("voice-a")).unwrap();
        std::fs::write(root.join("voice-a/weights.bin"), b"fixture").unwrap();
        storage::models::insert_model_install(
            &pool,
            &ModelInstall {
                model_id: "voice-a".into(),
                path: "voice-a".into(),
                sha256: "fixture".into(),
                disk_bytes: 7,
                source: "fixture".into(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
        storage::settings::set_setting(&pool, "default_model.tts", "voice-a")
            .await
            .unwrap();
        storage::settings::set_setting(&pool, "default_model.llm", "brain-b")
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER refuse_default_cleanup BEFORE DELETE ON app_setting WHEN OLD.key = 'default_model.tts' BEGIN SELECT RAISE(FAIL, 'cleanup refused'); END").execute(&pool).await.unwrap();
        let app = tauri::test::mock_app();
        app.manage(Arc::new(ModelManager::new(root.clone(), pool.clone())));
        app.manage(pool.clone());
        let error = delete_model(app.state(), app.state(), "voice-a".into())
            .await
            .unwrap_err();
        assert!(error.contains("cleanup refused"));
        assert!(!root.join("voice-a").exists());
        assert!(storage::models::get_model_install(&pool, "voice-a")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            storage::settings::get_setting(&pool, "default_model.tts")
                .await
                .unwrap()
                .as_deref(),
            Some("voice-a")
        );
        sqlx::query("DROP TRIGGER refuse_default_cleanup")
            .execute(&pool)
            .await
            .unwrap();
        for _ in 0..2 {
            delete_model(app.state(), app.state(), "voice-a".into())
                .await
                .unwrap();
        }
        assert_eq!(
            storage::settings::get_setting(&pool, "default_model.tts")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            storage::settings::get_setting(&pool, "default_model.llm")
                .await
                .unwrap()
                .as_deref(),
            Some("brain-b")
        );
    }

    #[tokio::test]
    async fn a_removal_io_failure_retains_the_install_and_its_default() {
        let dir = tempfile::tempdir().unwrap();
        let pool = storage::open_pool(&dir.path().join("models.sqlite"))
            .await
            .unwrap();
        let root = dir.path().join("models");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("voice-a"), b"not a directory").unwrap();
        storage::models::insert_model_install(
            &pool,
            &ModelInstall {
                model_id: "voice-a".into(),
                path: "voice-a".into(),
                sha256: "fixture".into(),
                disk_bytes: 15,
                source: "fixture".into(),
                installed_at: 1,
            },
        )
        .await
        .unwrap();
        storage::settings::set_setting(&pool, "default_model.tts", "voice-a")
            .await
            .unwrap();
        let app = tauri::test::mock_app();
        app.manage(Arc::new(ModelManager::new(root.clone(), pool.clone())));
        app.manage(pool.clone());
        assert!(delete_model(app.state(), app.state(), "voice-a".into())
            .await
            .is_err());
        assert!(storage::models::get_model_install(&pool, "voice-a")
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            storage::settings::get_setting(&pool, "default_model.tts")
                .await
                .unwrap()
                .as_deref(),
            Some("voice-a")
        );
        assert!(root.join("voice-a").is_file());
    }
}
