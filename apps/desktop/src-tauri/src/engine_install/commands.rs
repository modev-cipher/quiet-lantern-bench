use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use super::install::{
    install, space_needed, staged_bytes, CommitGuard, EngineHost, InstallError, InstalledEngine,
    Layout, Phase, Request, REQUIRED_ENGINE_API,
};
use super::manifest::{verify_and_parse_engine, EngineManifest, CUDA13_VARIANT, LITE_VARIANT};
use super::select::{choose, probe_nvidia, Choice};
use crate::sidecar::protocol::EngineStatus;
use crate::sidecar::supervisor::SupervisorHandle;
use crate::updater::manifest::{redirect_policy, url_is_acceptable, MANIFEST_MAX_BYTES};
use crate::updater::signing::PublicKey;

pub const EVENT: &str = "engine-install";
pub const DEFAULT_ENGINE_MANIFEST_URL: &str =
    "https://github.com/torkiv-labs/txtcaster-releases/releases/latest/download/engine.json";

#[derive(Default)]
pub struct EngineInstaller {
    running: Mutex<Option<Arc<AtomicBool>>>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum InstallEvent {
    #[serde(rename_all = "camelCase")]
    Downloading {
        bytes_done: u64,
        bytes_total: u64,
        speed_bps: u64,
    },
    Verifying,
    #[serde(rename_all = "camelCase")]
    Extracting {
        files_done: u64,
    },
    Starting,
    #[serde(rename_all = "camelCase")]
    Installed {
        engine: InstalledEngine,
    },
    #[serde(rename_all = "camelCase")]
    Failed {
        code: String,
        message: String,
    },
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VariantOffer {
    pub variant: String,
    pub download_bytes: u64,
    pub installed_bytes: u64,
    pub space_needed_bytes: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EngineInstallInfo {
    pub installed: Option<InstalledEngine>,
    pub required_api: u32,
    pub platform: String,
    pub offered: Vec<VariantOffer>,
    pub engine_version: Option<String>,
    pub recommended: Choice,
    pub space_available_bytes: Option<u64>,
    pub offer_error: Option<String>,
    pub installing: bool,
    pub lite_available: bool,
}

fn public_key() -> Option<PublicKey> {
    option_env!("TXTCASTER_UPDATE_PUBKEY").and_then(|text| PublicKey::from_base64(text).ok())
}

fn manifest_url() -> Option<reqwest::Url> {
    let text = std::env::var("TXTCASTER_ENGINE_MANIFEST_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| option_env!("TXTCASTER_ENGINE_MANIFEST_URL").map(str::to_string))
        .unwrap_or_else(|| DEFAULT_ENGINE_MANIFEST_URL.to_string());
    reqwest::Url::parse(text.trim())
        .ok()
        .filter(url_is_acceptable)
}

fn client_for(origin: &reqwest::Url) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("txtCaster/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60))
        .redirect(redirect_policy(origin))
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch_manifest(key: &PublicKey) -> Result<(reqwest::Client, EngineManifest), String> {
    let url = manifest_url().ok_or("ERR_ENGINE_NOT_CONFIGURED: no engine source is set")?;
    let client = client_for(&url)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("ERR_ENGINE_OFFLINE: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "ERR_ENGINE_OFFLINE: the engine list answered {}",
            response.status().as_u16()
        ));
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("ERR_ENGINE_OFFLINE: {e}"))?;
        if bytes.len() + chunk.len() > MANIFEST_MAX_BYTES {
            return Err("ERR_ENGINE_VERIFY: the engine list is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let manifest =
        verify_and_parse_engine(&bytes, key).map_err(|e| format!("ERR_ENGINE_VERIFY: {e}"))?;
    Ok((client, manifest))
}

fn platform() -> String {
    crate::updater::manifest::platform_key()
}

fn layout(app: &AppHandle) -> Result<Layout, String> {
    Ok(Layout {
        data_dir: crate::storage::paths::resolve_app_data_dir(app)?,
    })
}

pub fn init(app: &AppHandle, data_dir: &std::path::Path) {
    Layout {
        data_dir: data_dir.to_path_buf(),
    }
    .recover();
    app.manage(EngineInstaller::default());
}

/// The build behind the running engine, for local gating without any network request.
#[tauri::command]
pub fn engine_active_variant(app: AppHandle) -> Result<Option<String>, String> {
    let layout = layout(&app)?;
    if !super::capability::lite_engine_active(&layout.data_dir) {
        return Ok(None);
    }
    Ok(layout.installed().map(|engine| engine.variant))
}

#[tauri::command]
pub async fn engine_install_info(
    app: AppHandle,
    installer: State<'_, EngineInstaller>,
) -> Result<EngineInstallInfo, String> {
    let layout = layout(&app)?;
    let platform = platform();
    let gpus = probe_nvidia().await;
    let space = fs4::available_space(&layout.data_dir).ok();
    let installing = installer
        .running
        .lock()
        .map(|r| r.is_some())
        .unwrap_or(false);
    let (offered, engine_version, offer_error) = match public_key() {
        None => (
            Vec::new(),
            None,
            Some("ERR_ENGINE_NOT_CONFIGURED: this build cannot verify engine downloads".into()),
        ),
        Some(key) => match fetch_manifest(&key).await {
            Ok((_, manifest)) if manifest.engine_api != REQUIRED_ENGINE_API => (
                Vec::new(),
                Some(manifest.engine_version),
                Some("ERR_ENGINE_INCOMPATIBLE: no engine for this app version yet".into()),
            ),
            Ok((_, manifest)) => {
                let offers = manifest
                    .platforms
                    .get(&platform)
                    .map(|variants| {
                        variants
                            .iter()
                            .map(|(variant, archive)| VariantOffer {
                                variant: variant.clone(),
                                download_bytes: archive.size,
                                installed_bytes: archive.unpacked_size,
                                space_needed_bytes: space_needed(
                                    archive,
                                    staged_bytes(&layout.staging(variant, &archive.sha256)),
                                ),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (offers, Some(manifest.engine_version), None)
            }
            Err(error) => (Vec::new(), None, Some(error)),
        },
    };
    let cuda_offered = offered.iter().any(|o| o.variant == CUDA13_VARIANT);
    let lite_available = offered.iter().any(|o| o.variant == LITE_VARIANT);
    Ok(EngineInstallInfo {
        installed: layout.installed(),
        required_api: REQUIRED_ENGINE_API,
        platform,
        offered,
        engine_version,
        recommended: choose(&gpus, cuda_offered),
        space_available_bytes: space,
        offer_error,
        installing,
        lite_available,
    })
}

pub async fn hold_job_admission<R: tauri::Runtime>(app: &AppHandle<R>) -> CommitGuard {
    match app.try_state::<Arc<crate::orchestrator::worker::JobOrchestrator>>() {
        Some(orchestrator) => Some(orchestrator.lock_mutation_gate_owned().await),
        None => None,
    }
}

fn active_jobs_block(app: &AppHandle) -> impl std::future::Future<Output = Result<(), String>> {
    let pool = app.state::<crate::storage::SqlitePool>().inner().clone();
    async move {
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM render_job WHERE UPPER(state) NOT IN ('DONE', 'FAILED', 'CANCELLED', 'CANCELED')",
        )
        .fetch_one(&pool)
        .await
        .map_err(|e| e.to_string())?;
        if active > 0 {
            return Err(
                "ERR_JOB_ACTIVE: casts are running; finish or cancel them before changing the engine"
                    .into(),
            );
        }
        Ok(())
    }
}

/// Starts the install in the background and returns at once; progress and the outcome
/// arrive as `engine-install` events.
#[tauri::command]
pub async fn install_engine(
    app: AppHandle,
    installer: State<'_, EngineInstaller>,
    variant: String,
) -> Result<(), String> {
    let key = public_key()
        .ok_or("ERR_ENGINE_NOT_CONFIGURED: this build cannot verify engine downloads")?;
    active_jobs_block(&app).await?;
    let cancel = {
        let mut running = installer.running.lock().map_err(|e| e.to_string())?;
        if running.is_some() {
            return Err("ERR_ENGINE_BUSY: the engine is already being installed".into());
        }
        let flag = Arc::new(AtomicBool::new(false));
        *running = Some(Arc::clone(&flag));
        flag
    };
    let layout = layout(&app)?;
    tauri::async_runtime::spawn(async move {
        let outcome = run_install(&app, &layout, &key, &variant, &cancel).await;
        let event = match outcome {
            Ok(engine) => InstallEvent::Installed { engine },
            Err((code, message)) => InstallEvent::Failed { code, message },
        };
        if let Some(state) = app.try_state::<EngineInstaller>() {
            if let Ok(mut running) = state.running.lock() {
                *running = None;
            }
        }
        let _ = app.emit(EVENT, &event);
    });
    Ok(())
}

async fn run_install(
    app: &AppHandle,
    layout: &Layout,
    key: &PublicKey,
    variant: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<InstalledEngine, (String, String)> {
    let (client, manifest) = fetch_manifest(key).await.map_err(|e| {
        let (code, message) = e
            .split_once(": ")
            .unwrap_or(("ERR_ENGINE_OFFLINE", e.as_str()));
        (code.to_string(), message.to_string())
    })?;
    let platform = platform();
    let host = AppEngineHost { app: app.clone() };
    let started = Instant::now();
    let emitter = app.clone();
    install(
        &client,
        layout,
        Request {
            manifest: &manifest,
            platform: &platform,
            variant,
            key,
        },
        &host,
        cancel,
        |path| fs4::available_space(path),
        move |phase| {
            let event = match phase {
                Phase::Downloading { done, total } => {
                    let secs = started.elapsed().as_secs().max(1);
                    InstallEvent::Downloading {
                        bytes_done: done,
                        bytes_total: total,
                        speed_bps: done / secs,
                    }
                }
                Phase::Verifying => InstallEvent::Verifying,
                Phase::Extracting { files } => InstallEvent::Extracting { files_done: files },
                Phase::Starting => InstallEvent::Starting,
            };
            let _ = emitter.emit(EVENT, &event);
        },
    )
    .await
    .map_err(|e: InstallError| (e.code().to_string(), e.to_string()))
}

#[tauri::command]
pub fn cancel_engine_install(installer: State<'_, EngineInstaller>) -> bool {
    match installer.running.lock() {
        Ok(running) => match running.as_ref() {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        },
        Err(_) => false,
    }
}

struct AppEngineHost {
    app: AppHandle,
}

#[async_trait::async_trait]
impl EngineHost for AppEngineHost {
    async fn begin_commit(&self) -> Result<CommitGuard, String> {
        let gate = hold_job_admission(&self.app).await;
        active_jobs_block(&self.app).await?;
        Ok(gate)
    }

    async fn stop_engine(&self) -> bool {
        let supervisor = self.app.state::<SupervisorHandle>();
        supervisor.request_engine_shutdown().await;
        supervisor.stop_sidecar(Duration::from_secs(10)).await
    }

    fn start_engine(&self) {
        self.app.state::<SupervisorHandle>().respawn();
    }

    async fn check_started(&self, variant: &str, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let client = reqwest::Client::new();
        loop {
            let supervisor = self.app.state::<SupervisorHandle>();
            match supervisor.status() {
                EngineStatus::NeedsUpdate { installed_api, .. } => {
                    return Err(format!("the engine reports version {installed_api:?}"));
                }
                EngineStatus::NotInstalled => return Err("the engine was not found".into()),
                _ => {}
            }
            if let Some((port, token)) = supervisor.connection() {
                let health: Option<serde_json::Value> = async {
                    client
                        .get(format!("http://127.0.0.1:{port}/health"))
                        .bearer_auth(&token)
                        .timeout(Duration::from_secs(5))
                        .send()
                        .await
                        .ok()?
                        .json()
                        .await
                        .ok()
                }
                .await;
                if let Some(health) = health {
                    if variant != CUDA13_VARIANT {
                        return Ok(());
                    }
                    match health
                        .get("torch_cuda")
                        .and_then(serde_json::Value::as_bool)
                    {
                        Some(true) => return Ok(()),
                        Some(false) => {
                            return Err("the GPU build started but cannot use the GPU".into())
                        }
                        None => {}
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err("the engine did not answer in time".into());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::worker::{JobOrchestrator, StageRegistry, StartOptions};

    #[tokio::test]
    async fn an_engine_change_holds_job_admission_until_it_ends() {
        let dir = tempfile::tempdir().expect("dir");
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .expect("pool");
        let orchestrator = Arc::new(JobOrchestrator::start_with_options(
            pool,
            StageRegistry::default(),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
            Duration::from_millis(10),
            dir.path().to_path_buf(),
            StartOptions {
                consume_queue: false,
                ..Default::default()
            },
        ));
        let app = tauri::test::mock_app();
        app.manage(orchestrator.clone());

        let gate = hold_job_admission(app.handle()).await;
        assert!(gate.is_some());
        let admission =
            tokio::time::timeout(Duration::from_millis(50), orchestrator.lock_mutation_gate());
        assert!(admission.await.is_err());
        drop(gate);
        let _admitted =
            tokio::time::timeout(Duration::from_secs(1), orchestrator.lock_mutation_gate())
                .await
                .expect("admission resumes");
    }
}
