use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::fetch::{fetch_parts, whole_digest, FetchError};
use super::manifest::{EngineArchive, EngineManifest, CUDA13_VARIANT};
use super::unpack::{unpack_parts, UnpackError};
use crate::updater::signing::PublicKey;

pub const REQUIRED_ENGINE_API: u32 = 1;
pub const ENGINE_DIR: &str = "engine";
pub const MARKER_FILE: &str = "install.json";
const MIN_SPACE_MARGIN: u64 = 64 * 1024 * 1024;
const MAX_SPACE_MARGIN: u64 = 512 * 1024 * 1024;
const UNPACK_SLACK: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledEngine {
    pub engine_version: String,
    pub engine_api: u32,
    pub variant: String,
    pub platform: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Downloading { done: u64, total: u64 },
    Verifying,
    Extracting { files: u64 },
    Starting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    NotOffered { platform: String, variant: String },
    IncompatibleApi { offered: u32, required: u32 },
    NoSpace { needed: u64, available: u64 },
    Fetch(FetchError),
    Signature,
    Unpack(UnpackError),
    JobsActive(String),
    StopFailed,
    StartFailed(String),
    Io(String),
    Cancelled,
}

impl InstallError {
    pub fn code(&self) -> &'static str {
        match self {
            InstallError::NotOffered { .. } => "ERR_ENGINE_NOT_OFFERED",
            InstallError::IncompatibleApi { .. } => "ERR_ENGINE_INCOMPATIBLE",
            InstallError::NoSpace { .. } => "ERR_ENGINE_NO_SPACE",
            InstallError::Fetch(FetchError::Offline(_)) => "ERR_ENGINE_OFFLINE",
            InstallError::Fetch(FetchError::Cancelled) | InstallError::Cancelled => {
                "ERR_ENGINE_CANCELLED"
            }
            InstallError::Fetch(_) | InstallError::Signature => "ERR_ENGINE_VERIFY",
            InstallError::Unpack(UnpackError::Cancelled) => "ERR_ENGINE_CANCELLED",
            InstallError::Unpack(_) => "ERR_ENGINE_UNPACK",
            InstallError::JobsActive(_) => "ERR_JOB_ACTIVE",
            InstallError::StopFailed => "ERR_ENGINE_BUSY",
            InstallError::StartFailed(_) => "ERR_ENGINE_START",
            InstallError::Io(_) => "ERR_ENGINE_IO",
        }
    }
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NotOffered { platform, variant } => {
                write!(f, "no {variant} engine is offered for {platform}")
            }
            InstallError::IncompatibleApi { offered, required } => write!(
                f,
                "the offered engine speaks version {offered}, this app needs {required}"
            ),
            InstallError::NoSpace { needed, available } => write!(
                f,
                "not enough free space: {needed} bytes needed, {available} available"
            ),
            InstallError::Fetch(error) => write!(f, "{error}"),
            InstallError::Signature => write!(f, "the download is not signed by txtCaster"),
            InstallError::Unpack(error) => write!(f, "{error}"),
            InstallError::JobsActive(why) => write!(f, "{why}"),
            InstallError::StopFailed => {
                write!(f, "the running engine did not stop; nothing was changed")
            }
            InstallError::StartFailed(why) => write!(f, "the new engine did not start: {why}"),
            InstallError::Io(why) => write!(f, "the engine could not be installed: {why}"),
            InstallError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl From<std::io::Error> for InstallError {
    fn from(error: std::io::Error) -> Self {
        InstallError::Io(error.to_string())
    }
}

pub type CommitGuard = Option<tokio::sync::OwnedMutexGuard<()>>;

#[async_trait::async_trait]
pub trait EngineHost: Send + Sync {
    async fn begin_commit(&self) -> Result<CommitGuard, String>;
    async fn stop_engine(&self) -> bool;
    fn start_engine(&self);
    async fn check_started(&self, variant: &str, timeout: Duration) -> Result<(), String>;
}

pub struct Layout {
    pub data_dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct SwapJournal {
    had_previous: bool,
}

impl Layout {
    pub fn engine(&self) -> PathBuf {
        self.data_dir.join(ENGINE_DIR)
    }
    pub fn fresh(&self) -> PathBuf {
        self.data_dir.join("engine.new")
    }
    pub fn previous(&self) -> PathBuf {
        self.data_dir.join("engine.old")
    }
    pub fn failed(&self) -> PathBuf {
        self.data_dir.join("engine.failed")
    }
    pub fn journal(&self) -> PathBuf {
        self.data_dir.join("engine.swap")
    }
    pub fn staging_root(&self) -> PathBuf {
        self.data_dir.join("engine.partial")
    }
    pub fn staging(&self, variant: &str, sha256: &str) -> PathBuf {
        self.staging_root()
            .join(format!("{variant}-{}", &sha256[..sha256.len().min(16)]))
    }

    pub fn installed(&self) -> Option<InstalledEngine> {
        let bytes = std::fs::read(self.engine().join(MARKER_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn begin_swap(&self, had_previous: bool) -> std::io::Result<()> {
        let body =
            serde_json::to_vec(&SwapJournal { had_previous }).map_err(std::io::Error::other)?;
        let tmp = self.data_dir.join("engine.swap.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(tmp, self.journal())
    }

    fn end_swap(&self) -> std::io::Result<()> {
        match std::fs::remove_file(self.journal()) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    fn put_previous_back(&self) -> std::io::Result<()> {
        if self.engine().exists() {
            remove_if_exists(&self.failed())?;
            std::fs::rename(self.engine(), self.failed())?;
        }
        std::fs::rename(self.previous(), self.engine())
    }

    /// Repair what an interrupted install left behind. An engine swapped in but never
    /// confirmed (the journal is still there) is replaced by the one it displaced; the
    /// displaced copy is deleted only once the live engine is a confirmed one.
    pub fn recover(&self) {
        let journal: Option<SwapJournal> = std::fs::read(self.journal())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let mut previous_needed = false;
        if self.journal().exists() {
            let had_previous = journal.map_or(true, |j| j.had_previous);
            if self.previous().is_dir() {
                if self.put_previous_back().is_err() {
                    previous_needed = true;
                }
            } else if !had_previous {
                let _ = remove_if_exists(&self.engine());
            }
            if !previous_needed {
                let _ = self.end_swap();
            }
        } else if !self.engine().exists() && self.previous().is_dir() {
            previous_needed = std::fs::rename(self.previous(), self.engine()).is_err();
        }
        let _ = remove_if_exists(&self.fresh());
        let _ = remove_if_exists(&self.failed());
        let _ = std::fs::remove_file(self.data_dir.join("engine.swap.tmp"));
        if !previous_needed && self.engine().exists() {
            let _ = remove_if_exists(&self.previous());
        }
    }

    pub fn prune_staging_except(&self, keep: &Path) {
        let Ok(entries) = std::fs::read_dir(self.staging_root()) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            if entry.path() != keep {
                let _ = remove_if_exists(&entry.path());
            }
        }
    }
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

pub fn staged_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

pub fn space_needed(archive: &EngineArchive, already: u64) -> u64 {
    let margin = (archive.unpacked_size / 4).clamp(MIN_SPACE_MARGIN, MAX_SPACE_MARGIN);
    archive.size.saturating_sub(already) + archive.unpacked_size + margin
}

pub fn pick<'a>(
    manifest: &'a EngineManifest,
    platform: &str,
    variant: &str,
) -> Result<&'a EngineArchive, InstallError> {
    if manifest.engine_api != REQUIRED_ENGINE_API {
        return Err(InstallError::IncompatibleApi {
            offered: manifest.engine_api,
            required: REQUIRED_ENGINE_API,
        });
    }
    manifest
        .archive(platform, variant)
        .ok_or_else(|| InstallError::NotOffered {
            platform: platform.to_string(),
            variant: variant.to_string(),
        })
}

pub struct Request<'a> {
    pub manifest: &'a EngineManifest,
    pub platform: &'a str,
    pub variant: &'a str,
    pub key: &'a PublicKey,
}

async fn unpack_with_progress(
    parts: Vec<PathBuf>,
    fresh: PathBuf,
    cap: u64,
    cancel: &Arc<AtomicBool>,
    on_phase: &mut (dyn FnMut(Phase) + Send),
) -> Result<(), InstallError> {
    let files = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&files);
    let cancel_flag = Arc::clone(cancel);
    let target = fresh.clone();
    let mut job = tokio::task::spawn_blocking(move || {
        unpack_parts(&parts, &target, cap, &cancel_flag, |stats| {
            counter.store(stats.files, Ordering::Relaxed)
        })
    });
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let result = loop {
        tokio::select! {
            joined = &mut job => break joined,
            _ = tick.tick() => on_phase(Phase::Extracting { files: files.load(Ordering::Relaxed) }),
        }
    };
    on_phase(Phase::Extracting {
        files: files.load(Ordering::Relaxed),
    });
    match result {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => {
            let _ = remove_if_exists(&fresh);
            Err(match error {
                UnpackError::Cancelled => InstallError::Cancelled,
                other => InstallError::Unpack(other),
            })
        }
        Err(join) => {
            let _ = remove_if_exists(&fresh);
            Err(InstallError::Io(join.to_string()))
        }
    }
}

/// Windows keeps a stopped process's image and antivirus scans open for moments after exit,
/// and refuses the folder rename with a sharing violation until they let go.
fn rename_with_patience(from: &Path, to: &Path) -> std::io::Result<()> {
    let attempts = if cfg!(windows) { 20 } else { 1 };
    for attempt in 1..=attempts {
        match std::fs::rename(from, to) {
            Err(err)
                if attempt < attempts && err.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            other => return other,
        }
    }
    unreachable!("the last attempt always returns")
}

fn swap_in(layout: &Layout, had_previous: bool) -> std::io::Result<()> {
    remove_if_exists(&layout.previous())?;
    layout.begin_swap(had_previous)?;
    if had_previous {
        rename_with_patience(&layout.engine(), &layout.previous())?;
    }
    rename_with_patience(&layout.fresh(), &layout.engine())
}

fn roll_back(layout: &Layout, had_previous: bool) -> Result<(), String> {
    if had_previous {
        if layout.previous().is_dir() {
            layout
                .put_previous_back()
                .map_err(|e| format!("the previous engine could not be restored: {e}"))?;
        }
    } else {
        let _ = remove_if_exists(&layout.engine());
    }
    let _ = remove_if_exists(&layout.failed());
    let _ = remove_if_exists(&layout.fresh());
    layout
        .end_swap()
        .map_err(|e| format!("the install record could not be cleared: {e}"))
}

/// Download, verify, unpack and switch to the requested engine. The engine that was
/// installed before stays in place until the new one has started and answered; if it
/// does not, the old one is put back and started again.
pub async fn install(
    client: &reqwest::Client,
    layout: &Layout,
    request: Request<'_>,
    host: &dyn EngineHost,
    cancel: &Arc<AtomicBool>,
    available_space: impl Fn(&Path) -> std::io::Result<u64> + Send + Sync,
    mut on_phase: impl FnMut(Phase) + Send,
) -> Result<InstalledEngine, InstallError> {
    let archive = pick(request.manifest, request.platform, request.variant)?;
    std::fs::create_dir_all(&layout.data_dir)?;
    layout.recover();
    let staging = layout.staging(request.variant, &archive.sha256);
    layout.prune_staging_except(&staging);
    let needed = space_needed(archive, staged_bytes(&staging));
    let available = available_space(&layout.data_dir)?;
    if available < needed {
        return Err(InstallError::NoSpace { needed, available });
    }

    let parts = fetch_parts(client, archive, &staging, cancel, |done, total| {
        on_phase(Phase::Downloading { done, total })
    })
    .await
    .map_err(|e| match e {
        FetchError::Cancelled => InstallError::Cancelled,
        other => InstallError::Fetch(other),
    })?;

    on_phase(Phase::Verifying);
    let digest = {
        let parts = parts.clone();
        tokio::task::spawn_blocking(move || whole_digest(&parts))
            .await
            .map_err(|e| InstallError::Io(e.to_string()))??
    };
    if hex::encode(digest) != archive.sha256.to_lowercase()
        || request
            .key
            .verify_engine_archive_digest(&digest, &archive.signature)
            .is_err()
    {
        let _ = remove_if_exists(&staging);
        return Err(InstallError::Signature);
    }

    let fresh = layout.fresh();
    unpack_with_progress(
        parts,
        fresh.clone(),
        archive.unpacked_size + UNPACK_SLACK,
        cancel,
        &mut on_phase,
    )
    .await?;

    let installed = InstalledEngine {
        engine_version: request.manifest.engine_version.clone(),
        engine_api: request.manifest.engine_api,
        variant: request.variant.to_string(),
        platform: request.platform.to_string(),
    };
    let marker = serde_json::to_vec_pretty(&installed).map_err(|e| InstallError::Io(e.to_string()));
    if let Err(error) = marker.and_then(|bytes| Ok(std::fs::write(fresh.join(MARKER_FILE), bytes)?))
    {
        let _ = remove_if_exists(&fresh);
        return Err(error);
    }
    if cancel.load(Ordering::Relaxed) {
        let _ = remove_if_exists(&fresh);
        return Err(InstallError::Cancelled);
    }

    let _gate = match host.begin_commit().await {
        Ok(gate) => gate,
        Err(why) => {
            let _ = remove_if_exists(&fresh);
            return Err(InstallError::JobsActive(why));
        }
    };
    if cancel.load(Ordering::Relaxed) {
        let _ = remove_if_exists(&fresh);
        return Err(InstallError::Cancelled);
    }

    on_phase(Phase::Starting);
    if !host.stop_engine().await {
        let _ = remove_if_exists(&fresh);
        host.start_engine();
        return Err(InstallError::StopFailed);
    }
    let had_previous = layout.engine().exists();
    let swap_layout = Layout {
        data_dir: layout.data_dir.clone(),
    };
    let swapped = tokio::task::spawn_blocking(move || swap_in(&swap_layout, had_previous))
        .await
        .unwrap_or_else(|join| Err(std::io::Error::other(join)));
    if let Err(error) = swapped {
        let recovery = roll_back(layout, had_previous);
        host.start_engine();
        return Err(match recovery {
            Ok(()) => InstallError::Io(error.to_string()),
            Err(also) => InstallError::Io(format!("{error}; {also}")),
        });
    }
    host.start_engine();
    let timeout = if request.variant == CUDA13_VARIANT {
        Duration::from_secs(240)
    } else {
        Duration::from_secs(120)
    };
    match host.check_started(request.variant, timeout).await {
        Ok(()) => {
            layout.end_swap()?;
            let _ = remove_if_exists(&layout.previous());
            let _ = remove_if_exists(&layout.staging_root());
            Ok(installed)
        }
        Err(why) => {
            host.stop_engine().await;
            let recovery = roll_back(layout, had_previous);
            host.start_engine();
            Err(InstallError::StartFailed(match recovery {
                Ok(()) => why,
                Err(also) => format!("{why}; {also}"),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::engine_install::fetch::fixture::{serve, Served};
    use crate::engine_install::manifest::{EnginePart, CPU_VARIANT};
    use crate::engine_install::pack::pack_engine;
    use crate::updater::signing::SigningKey;

    const BIN: &str = if cfg!(windows) {
        "txtcaster-engine.exe"
    } else {
        "txtcaster-engine"
    };

    #[derive(Default)]
    struct FakeHost {
        log: Mutex<Vec<String>>,
        refuse: bool,
        jobs_running: bool,
        stuck: bool,
    }

    #[async_trait::async_trait]
    impl EngineHost for FakeHost {
        async fn begin_commit(&self) -> Result<CommitGuard, String> {
            self.log.lock().expect("log").push("gate".into());
            if self.jobs_running {
                Err("casts are running".into())
            } else {
                Ok(None)
            }
        }
        async fn stop_engine(&self) -> bool {
            self.log.lock().expect("log").push("stop".into());
            !self.stuck
        }
        fn start_engine(&self) {
            self.log.lock().expect("log").push("start".into());
        }
        async fn check_started(&self, variant: &str, _timeout: Duration) -> Result<(), String> {
            self.log
                .lock()
                .expect("log")
                .push(format!("check {variant}"));
            if self.refuse {
                Err("the GPU build cannot see a GPU".into())
            } else {
                Ok(())
            }
        }
    }

    struct Release {
        _served: tempfile::TempDir,
        manifest: EngineManifest,
        key: SigningKey,
        state: Arc<Mutex<Served>>,
    }

    async fn release(engine_api: u32, marker: &[u8]) -> Release {
        let src = tempfile::tempdir().expect("src");
        std::fs::write(src.path().join(BIN), marker).expect("bin");
        std::fs::create_dir_all(src.path().join("_internal")).expect("dir");
        std::fs::write(src.path().join("_internal/lib.bin"), vec![3u8; 4096]).expect("lib");
        let out = tempfile::tempdir().expect("out");
        let packed = pack_engine(src.path(), &out.path().join("e"), 1500, 1, 1).expect("pack");
        let state = Arc::new(Mutex::new(Served::default()));
        let base = serve(Arc::clone(&state)).await;
        let key = SigningKey::generate().expect("key").1;
        let mut parts = Vec::new();
        for (index, part) in packed.parts.iter().enumerate() {
            let name = format!("/p.{index:03}");
            state
                .lock()
                .expect("lock")
                .files
                .insert(name.clone(), std::fs::read(&part.path).expect("part"));
            parts.push(EnginePart {
                url: format!("{base}{name}"),
                size: part.size,
                sha256: hex::encode(part.sha256),
            });
        }
        let archive = EngineArchive {
            size: packed.size,
            sha256: hex::encode(packed.sha256),
            signature: key.sign_engine_archive_digest(&packed.sha256),
            unpacked_size: packed.unpacked_size,
            parts,
        };
        let manifest = EngineManifest {
            engine_version: "0.1.0".into(),
            engine_api,
            platforms: BTreeMap::from([(
                "linux-x86_64".to_string(),
                BTreeMap::from([(CPU_VARIANT.to_string(), archive)]),
            )]),
        };
        Release {
            _served: out,
            manifest,
            key,
            state,
        }
    }

    fn plenty(_: &Path) -> std::io::Result<u64> {
        Ok(u64::MAX)
    }

    async fn run(
        release: &Release,
        layout: &Layout,
        host: &FakeHost,
        key: &PublicKey,
    ) -> Result<InstalledEngine, InstallError> {
        install(
            &reqwest::Client::new(),
            layout,
            Request {
                manifest: &release.manifest,
                platform: "linux-x86_64",
                variant: CPU_VARIANT,
                key,
            },
            host,
            &Arc::new(AtomicBool::new(false)),
            plenty,
            |_| {},
        )
        .await
    }

    fn archive_sized(size: u64, unpacked_size: u64) -> EngineArchive {
        EngineArchive {
            size,
            sha256: String::new(),
            signature: String::new(),
            unpacked_size,
            parts: Vec::new(),
        }
    }

    #[test]
    fn the_space_margin_scales_with_the_package() {
        const MIB: u64 = 1024 * 1024;
        let reader = archive_sized(34 * MIB, 92 * MIB);
        assert_eq!(space_needed(&reader, 0), (34 + 92 + 64) * MIB);
        let cpu = archive_sized(519 * MIB, 1700 * MIB);
        assert_eq!(space_needed(&cpu, 0), (519 + 1700 + 425) * MIB);
        let graphics = archive_sized(2150 * MIB, 6000 * MIB);
        assert_eq!(space_needed(&graphics, 0), (2150 + 6000 + 512) * MIB);
    }

    #[tokio::test]
    async fn a_first_install_lands_in_the_engine_folder_with_its_marker() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = Layout {
            data_dir: data.path().to_path_buf(),
        };
        let host = FakeHost::default();
        let installed = run(&rel, &layout, &host, &rel.key.public_key())
            .await
            .expect("install");
        assert_eq!(installed.variant, CPU_VARIANT);
        assert_eq!(layout.installed(), Some(installed));
        assert_eq!(
            std::fs::read(layout.engine().join(BIN)).expect("bin"),
            b"new engine"
        );
        assert!(!layout.previous().exists());
        assert!(!layout.fresh().exists());
        assert!(!layout.staging_root().exists());
        assert_eq!(
            *host.log.lock().expect("log"),
            vec![
                "gate".to_string(),
                "stop".into(),
                "start".into(),
                "check cpu".into()
            ]
        );
    }

    #[tokio::test]
    async fn the_app_launches_an_installed_engine_from_the_engine_folder() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = Layout {
            data_dir: data.path().to_path_buf(),
        };
        run(&rel, &layout, &FakeHost::default(), &rel.key.public_key())
            .await
            .expect("install");
        let nowhere = data.path().join("no-exe-dir");
        match crate::sidecar::spawn::resolve_launch(Some(data.path()), &nowhere, None, None) {
            Some(crate::sidecar::spawn::SidecarLaunch::Packaged { binary }) => {
                assert_eq!(binary, layout.engine().join(BIN));
            }
            other => panic!("installed engine not launched: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_engine_that_does_not_start_is_rolled_back_to_the_previous_one() {
        let rel = release(REQUIRED_ENGINE_API, b"broken engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = Layout {
            data_dir: data.path().to_path_buf(),
        };
        std::fs::create_dir_all(layout.engine()).expect("old");
        std::fs::write(layout.engine().join(BIN), b"old engine").expect("old bin");
        let host = FakeHost {
            refuse: true,
            ..FakeHost::default()
        };
        let result = run(&rel, &layout, &host, &rel.key.public_key()).await;
        assert!(
            matches!(result, Err(InstallError::StartFailed(_))),
            "{result:?}"
        );
        assert_eq!(
            std::fs::read(layout.engine().join(BIN)).expect("bin"),
            b"old engine"
        );
        assert!(!layout.failed().exists());
        assert_eq!(
            host.log.lock().expect("log").last().map(String::as_str),
            Some("start")
        );
    }

    #[tokio::test]
    async fn a_download_signed_by_another_key_never_replaces_the_engine() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = Layout {
            data_dir: data.path().to_path_buf(),
        };
        let stranger = SigningKey::generate().expect("key").1;
        let host = FakeHost::default();
        let result = run(&rel, &layout, &host, &stranger.public_key()).await;
        assert_eq!(result, Err(InstallError::Signature));
        assert!(!layout.engine().exists());
        assert!(
            host.log.lock().expect("log").is_empty(),
            "the engine is never stopped"
        );
    }

    #[tokio::test]
    async fn an_engine_for_another_app_version_is_refused_before_downloading() {
        let rel = release(REQUIRED_ENGINE_API + 1, b"future engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = Layout {
            data_dir: data.path().to_path_buf(),
        };
        let result = run(&rel, &layout, &FakeHost::default(), &rel.key.public_key()).await;
        assert_eq!(
            result,
            Err(InstallError::IncompatibleApi {
                offered: REQUIRED_ENGINE_API + 1,
                required: REQUIRED_ENGINE_API
            })
        );
        assert!(rel.state.lock().expect("lock").requests.is_empty());
    }

    #[tokio::test]
    async fn not_enough_space_is_refused_before_downloading() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = Layout {
            data_dir: data.path().to_path_buf(),
        };
        let result = install(
            &reqwest::Client::new(),
            &layout,
            Request {
                manifest: &rel.manifest,
                platform: "linux-x86_64",
                variant: CPU_VARIANT,
                key: &rel.key.public_key(),
            },
            &FakeHost::default(),
            &Arc::new(AtomicBool::new(false)),
            |_| Ok(1024),
            |_| {},
        )
        .await;
        assert!(
            matches!(result, Err(InstallError::NoSpace { .. })),
            "{result:?}"
        );
        assert!(rel.state.lock().expect("lock").requests.is_empty());
    }

    #[tokio::test]
    async fn a_variant_the_release_does_not_offer_is_refused() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let result = install(
            &reqwest::Client::new(),
            &Layout {
                data_dir: data.path().to_path_buf(),
            },
            Request {
                manifest: &rel.manifest,
                platform: "linux-x86_64",
                variant: CUDA13_VARIANT,
                key: &rel.key.public_key(),
            },
            &FakeHost::default(),
            &Arc::new(AtomicBool::new(false)),
            plenty,
            |_| {},
        )
        .await;
        assert!(
            matches!(result, Err(InstallError::NotOffered { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_crash_mid_swap_puts_the_previous_engine_back_at_launch() {
        let data = tempfile::tempdir().expect("data");
        let layout = Layout {
            data_dir: data.path().to_path_buf(),
        };
        std::fs::create_dir_all(layout.previous()).expect("old");
        std::fs::write(layout.previous().join(BIN), b"old engine").expect("old bin");
        std::fs::create_dir_all(layout.fresh()).expect("new");
        layout.recover();
        assert_eq!(
            std::fs::read(layout.engine().join(BIN)).expect("bin"),
            b"old engine"
        );
        assert!(!layout.previous().exists());
        assert!(!layout.fresh().exists());
    }

    fn layout_in(dir: &tempfile::TempDir) -> Layout {
        Layout {
            data_dir: dir.path().to_path_buf(),
        }
    }

    fn old_engine(layout: &Layout) {
        std::fs::create_dir_all(layout.engine()).expect("old");
        std::fs::write(layout.engine().join(BIN), b"old engine").expect("old bin");
    }

    #[test]
    fn an_unconfirmed_swap_found_at_launch_restores_the_engine_it_replaced() {
        let data = tempfile::tempdir().expect("data");
        let layout = layout_in(&data);
        old_engine(&layout);
        std::fs::rename(layout.engine(), layout.previous()).expect("displace");
        std::fs::create_dir_all(layout.engine()).expect("new");
        std::fs::write(layout.engine().join(BIN), b"broken new engine").expect("new bin");
        layout.begin_swap(true).expect("journal");
        layout.recover();
        assert_eq!(
            std::fs::read(layout.engine().join(BIN)).expect("bin"),
            b"old engine"
        );
        assert!(!layout.previous().exists());
        assert!(!layout.journal().exists());
    }

    #[test]
    fn an_unconfirmed_first_install_found_at_launch_is_removed() {
        let data = tempfile::tempdir().expect("data");
        let layout = layout_in(&data);
        std::fs::create_dir_all(layout.engine()).expect("new");
        layout.begin_swap(false).expect("journal");
        layout.recover();
        assert!(!layout.engine().exists());
        assert!(!layout.journal().exists());
    }

    #[test]
    fn a_confirmed_engine_keeps_its_place_and_a_stray_copy_is_reclaimed() {
        let data = tempfile::tempdir().expect("data");
        let layout = layout_in(&data);
        old_engine(&layout);
        std::fs::create_dir_all(layout.previous()).expect("stray");
        layout.recover();
        assert_eq!(
            std::fs::read(layout.engine().join(BIN)).expect("bin"),
            b"old engine"
        );
        assert!(!layout.previous().exists());
    }

    #[tokio::test]
    async fn a_failure_after_stopping_the_engine_restores_and_restarts_it() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = layout_in(&data);
        old_engine(&layout);
        let fresh = layout.fresh();
        let host = FakeHost::default();
        let result = install(
            &reqwest::Client::new(),
            &layout,
            Request {
                manifest: &rel.manifest,
                platform: "linux-x86_64",
                variant: CPU_VARIANT,
                key: &rel.key.public_key(),
            },
            &host,
            &Arc::new(AtomicBool::new(false)),
            plenty,
            move |phase| {
                if phase == Phase::Starting {
                    std::fs::remove_dir_all(&fresh).expect("lose the new engine");
                }
            },
        )
        .await;
        assert!(matches!(result, Err(InstallError::Io(_))), "{result:?}");
        assert_eq!(
            std::fs::read(layout.engine().join(BIN)).expect("bin"),
            b"old engine"
        );
        assert!(!layout.previous().exists());
        assert!(!layout.journal().exists());
        assert_eq!(
            host.log.lock().expect("log").last().map(String::as_str),
            Some("start")
        );
    }

    #[tokio::test]
    async fn an_engine_that_will_not_stop_is_left_running_and_untouched() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = layout_in(&data);
        old_engine(&layout);
        let host = FakeHost {
            stuck: true,
            ..FakeHost::default()
        };
        let result = run(&rel, &layout, &host, &rel.key.public_key()).await;
        assert_eq!(result, Err(InstallError::StopFailed));
        assert_eq!(
            std::fs::read(layout.engine().join(BIN)).expect("bin"),
            b"old engine"
        );
        assert!(!layout.fresh().exists());
        assert_eq!(
            host.log.lock().expect("log").last().map(String::as_str),
            Some("start")
        );
    }

    #[tokio::test]
    async fn running_casts_stop_the_swap_before_the_engine_is_touched() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = layout_in(&data);
        old_engine(&layout);
        let host = FakeHost {
            jobs_running: true,
            ..FakeHost::default()
        };
        let result = run(&rel, &layout, &host, &rel.key.public_key()).await;
        assert!(
            matches!(result, Err(InstallError::JobsActive(_))),
            "{result:?}"
        );
        assert_eq!(*host.log.lock().expect("log"), vec!["gate".to_string()]);
        assert_eq!(
            std::fs::read(layout.engine().join(BIN)).expect("bin"),
            b"old engine"
        );
        assert!(!layout.fresh().exists());
    }

    #[tokio::test]
    async fn cancelling_after_unpacking_never_swaps_the_engine() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = layout_in(&data);
        old_engine(&layout);
        let host = FakeHost::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let result = install(
            &reqwest::Client::new(),
            &layout,
            Request {
                manifest: &rel.manifest,
                platform: "linux-x86_64",
                variant: CPU_VARIANT,
                key: &rel.key.public_key(),
            },
            &host,
            &cancel,
            plenty,
            move |phase| {
                if matches!(phase, Phase::Extracting { .. }) {
                    flag.store(true, Ordering::Relaxed);
                }
            },
        )
        .await;
        assert_eq!(result, Err(InstallError::Cancelled));
        assert!(
            host.log.lock().expect("log").is_empty(),
            "the engine is never stopped"
        );
        assert_eq!(
            std::fs::read(layout.engine().join(BIN)).expect("bin"),
            b"old engine"
        );
        assert!(!layout.fresh().exists());
    }

    #[tokio::test]
    async fn a_stale_extraction_is_reclaimed_before_the_space_check() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = layout_in(&data);
        std::fs::create_dir_all(layout.fresh()).expect("stale");
        std::fs::write(layout.fresh().join("leftover"), vec![0u8; 4096]).expect("stale file");
        let fresh = layout.fresh();
        let result = install(
            &reqwest::Client::new(),
            &layout,
            Request {
                manifest: &rel.manifest,
                platform: "linux-x86_64",
                variant: CPU_VARIANT,
                key: &rel.key.public_key(),
            },
            &FakeHost::default(),
            &Arc::new(AtomicBool::new(false)),
            move |_| {
                Ok(if fresh.join("leftover").exists() {
                    1
                } else {
                    u64::MAX
                })
            },
            |_| {},
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn downloads_of_other_builds_or_releases_are_reclaimed() {
        let rel = release(REQUIRED_ENGINE_API, b"new engine").await;
        let data = tempfile::tempdir().expect("data");
        let layout = layout_in(&data);
        let abandoned = layout.staging(CUDA13_VARIANT, &"ab".repeat(32));
        std::fs::create_dir_all(&abandoned).expect("abandoned");
        std::fs::write(abandoned.join("part.000"), vec![1u8; 2048]).expect("part");
        run(&rel, &layout, &FakeHost::default(), &rel.key.public_key())
            .await
            .expect("install");
        assert!(!abandoned.exists());
    }
}
