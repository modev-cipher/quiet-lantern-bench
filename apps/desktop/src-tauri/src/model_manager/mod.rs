use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};

use crate::catalog::{is_safe_path_segment, CatalogEntry, DownloadFile, ModelSource, ModelTask};
use crate::storage::{self, models};

const PART_SUFFIX: &str = ".part";

#[derive(Debug)]
pub enum ModelManagerError {
    UnknownModel(String),
    UnsafeModelId(String),
    UnsafeDestPath(String),
    AlreadyInstalled(String),
    NotInstalled(String),
    InsufficientDisk {
        needed: u64,
        available: u64,
    },
    Cancelled,
    HashMismatch {
        dest_path: String,
        expected: String,
        actual: String,
    },
    SizeMismatch {
        dest_path: String,
        expected: u64,
        actual: u64,
    },
    HttpStatus {
        status: u16,
        url: String,
    },
    Network(reqwest::Error),
    Io(std::io::Error),
    Db(sqlx::Error),
}

impl std::fmt::Display for ModelManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownModel(id) => write!(f, "model '{id}' not found in catalog"),
            Self::UnsafeModelId(id) => {
                write!(f, "model id '{id}' is not a safe path segment")
            }
            Self::UnsafeDestPath(p) => {
                write!(f, "dest path '{p}' escapes models root")
            }
            Self::AlreadyInstalled(id) => write!(f, "model '{id}' is already installed"),
            Self::NotInstalled(id) => write!(f, "model '{id}' is not installed"),
            Self::InsufficientDisk { needed, available } => write!(
                f,
                "insufficient disk space: needs {needed} bytes, {available} bytes free"
            ),
            Self::Cancelled => write!(f, "download cancelled"),
            Self::HashMismatch {
                dest_path,
                expected,
                actual,
            } => write!(
                f,
                "hash mismatch for '{dest_path}': expected {expected}, got {actual}"
            ),
            Self::SizeMismatch {
                dest_path,
                expected,
                actual,
            } => write!(
                f,
                "size mismatch for '{dest_path}': expected {expected} bytes, got {actual}"
            ),
            Self::HttpStatus { status, url } => {
                write!(f, "server returned status {status} for '{url}'")
            }
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for ModelManagerError {}

impl From<reqwest::Error> for ModelManagerError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e)
    }
}

impl From<std::io::Error> for ModelManagerError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<sqlx::Error> for ModelManagerError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

pub const MAX_DOWNLOAD_ATTEMPTS: u32 = 4;
const RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

impl ModelManagerError {
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network(e) => {
                e.is_timeout() || e.is_connect() || e.is_body() || e.is_decode() || e.is_request()
            }
            Self::HttpStatus { status, .. } => *status == 408 || *status == 429 || *status >= 500,
            Self::SizeMismatch {
                expected, actual, ..
            } => actual < expected,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub model_id: String,
    pub file_index: usize,
    pub files_total: usize,
    pub dest_path: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub state: DownloadState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Downloading,
    Reconnecting,
    Verifying,
    Done,
}

pub const RETIRED_MODEL_IDS: [&str; 8] = [
    "qwen3-vl-4b-q4km",
    "qwen3-vl-8b-q4km",
    "moondream-small",
    "minicpm-v-4.5",
    "xtts-v2",
    "piper-en-us-lessac-medium",
    "orpheus-3b",
    "f5-tts",
];

pub fn task_dir_name(task: ModelTask) -> &'static str {
    match task {
        ModelTask::Llm => "llm",
        ModelTask::Tts => "tts",
        ModelTask::Align => "align",
        ModelTask::Layout => "layout",
        ModelTask::Unknown => "unknown",
    }
}

pub fn install_rel_dir(entry: &CatalogEntry) -> String {
    format!("{}/{}", task_dir_name(entry.task), entry.id)
}

pub fn missing_files(entry: &CatalogEntry, install_dir: &Path) -> Vec<String> {
    entry
        .download
        .files
        .iter()
        .filter(|file| !install_dir.join(&file.dest_path).is_file())
        .map(|file| file.dest_path.clone())
        .collect()
}

pub fn manifest_hash(files: &[DownloadFile]) -> String {
    let mut lines: Vec<String> = files
        .iter()
        .map(|f| format!("{}:{}\n", f.dest_path, f.sha256))
        .collect();
    lines.sort();
    let mut hasher = Sha256::new();
    for line in &lines {
        hasher.update(line.as_bytes());
    }
    hex::encode(hasher.finalize())
}

pub struct ModelManager {
    models_root: PathBuf,
    pool: SqlitePool,
    client: reqwest::Client,
}

impl ModelManager {
    pub fn new(models_root: PathBuf, pool: SqlitePool) -> Self {
        let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url().clone();
            let origin = attempt.previous().first();
            let origin_host = origin.and_then(|o| o.host_str()).unwrap_or_default();
            let is_loopback =
                |h: &str| h == "127.0.0.1" || h == "localhost" || h == "[::1]" || h == "::1";
            let origin_is_loopback = is_loopback(origin_host);
            let host = url.host_str().unwrap_or_default();
            let scheme_ok =
                url.scheme() == "https" || (origin_is_loopback && url.scheme() == "http");
            let host_ok = host == origin_host
                || host.ends_with(".huggingface.co")
                || host.ends_with(".hf.co")
                || host == "huggingface.co"
                || host == "cdn-lfs.huggingface.co"
                || host == "cas-bridge.xethub.hf.co"
                || is_loopback(host);
            let hops = attempt.previous().len();
            if hops >= 5 {
                attempt.error("too many redirects")
            } else if scheme_ok && host_ok {
                attempt.follow()
            } else {
                attempt.error(format!("redirect to disallowed url {url}"))
            }
        });

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(60))
            .redirect(redirect_policy)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            models_root,
            pool,
            client,
        }
    }

    pub fn models_root(&self) -> &Path {
        &self.models_root
    }

    pub async fn list_installed(&self) -> Result<Vec<models::ModelInstall>, ModelManagerError> {
        Ok(models::list_model_installs(&self.pool).await?)
    }

    pub async fn delete(&self, model_id: &str) -> Result<(), ModelManagerError> {
        let install = models::get_model_install(&self.pool, model_id)
            .await?
            .ok_or_else(|| ModelManagerError::NotInstalled(model_id.to_string()))?;
        let dir = self.models_root.join(&install.path);
        if dir.starts_with(&self.models_root) && dir.exists() {
            tokio::fs::remove_dir_all(&dir).await?;
        }
        models::delete_model_install(&self.pool, model_id).await?;
        Ok(())
    }

    pub async fn purge_retired(&self) -> Vec<String> {
        let mut removed = Vec::new();
        for model_id in RETIRED_MODEL_IDS {
            match self.delete(model_id).await {
                Ok(()) => removed.push(model_id.to_string()),
                Err(ModelManagerError::NotInstalled(_)) => {}
                Err(err) => tracing::warn!("retired model '{model_id}' was not removed: {err}"),
            }
        }
        removed
    }

    pub async fn download(
        &self,
        entry: &CatalogEntry,
        progress: mpsc::Sender<DownloadProgress>,
        cancel: watch::Receiver<bool>,
    ) -> Result<(), ModelManagerError> {
        if !is_safe_path_segment(&entry.id) {
            return Err(ModelManagerError::UnsafeModelId(entry.id.clone()));
        }
        let rel_dir = install_rel_dir(entry);
        let install_dir = self.models_root.join(&rel_dir);
        if let Some(install) = models::get_model_install(&self.pool, &entry.id).await? {
            if install.sha256 == manifest_hash(&entry.download.files)
                && missing_files(entry, &install_dir).is_empty()
                && entry.download.files.iter().all(|f| {
                    install_dir
                        .join(&f.dest_path)
                        .metadata()
                        .map(|m| m.len() == f.size_bytes)
                        .unwrap_or(false)
                })
            {
                return Err(ModelManagerError::AlreadyInstalled(entry.id.clone()));
            }
        }
        tokio::fs::create_dir_all(&install_dir).await?;

        self.check_disk(entry, &install_dir).await?;

        let files_total = entry.download.files.len();
        for (file_index, file) in entry.download.files.iter().enumerate() {
            self.download_file(entry, file, file_index, files_total, &progress, &cancel)
                .await?;
        }

        let disk_bytes: u64 = entry.download.files.iter().map(|f| f.size_bytes).sum();
        let install = models::ModelInstall {
            model_id: entry.id.clone(),
            path: format!("{rel_dir}/"),
            sha256: manifest_hash(&entry.download.files),
            disk_bytes: disk_bytes as i64,
            source: match entry.source {
                ModelSource::Custom => "custom".to_string(),
                _ => "curated".to_string(),
            },
            installed_at: storage::now_ms(),
        };
        models::insert_model_install(&self.pool, &install).await?;
        Ok(())
    }

    async fn check_disk(
        &self,
        entry: &CatalogEntry,
        install_dir: &Path,
    ) -> Result<(), ModelManagerError> {
        let mut needed: u64 = 0;
        for file in &entry.download.files {
            let dest = install_dir.join(&file.dest_path);
            if !dest.starts_with(&self.models_root) {
                return Err(ModelManagerError::UnsafeDestPath(file.dest_path.clone()));
            }
            let part = part_path(&dest);
            let already = if let Ok(meta) = tokio::fs::metadata(&dest).await {
                if meta.is_file() && meta.len() == file.size_bytes {
                    file.size_bytes
                } else {
                    0
                }
            } else {
                tokio::fs::metadata(&part)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0)
            };
            needed += file.size_bytes.saturating_sub(already);
        }
        let available = fs4::available_space(install_dir)?;
        if needed > available {
            return Err(ModelManagerError::InsufficientDisk { needed, available });
        }
        Ok(())
    }

    async fn download_file(
        &self,
        entry: &CatalogEntry,
        file: &DownloadFile,
        file_index: usize,
        files_total: usize,
        progress: &mpsc::Sender<DownloadProgress>,
        cancel: &watch::Receiver<bool>,
    ) -> Result<(), ModelManagerError> {
        let install_dir = self.models_root.join(install_rel_dir(entry));
        let dest = install_dir.join(&file.dest_path);
        if !dest.starts_with(&self.models_root) {
            return Err(ModelManagerError::UnsafeDestPath(file.dest_path.clone()));
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        if dest.exists() {
            send_progress(
                progress,
                entry,
                file,
                file_index,
                files_total,
                0,
                DownloadState::Verifying,
            )
            .await;
            let mut hasher = Sha256::new();
            hash_into_with_progress(&dest, &mut hasher, |bytes_done| {
                try_send_progress(
                    progress,
                    entry,
                    file,
                    file_index,
                    files_total,
                    bytes_done,
                    DownloadState::Verifying,
                );
                !*cancel.borrow()
            })
            .await?;
            let actual = hex::encode(hasher.finalize());
            if actual == file.sha256 {
                send_progress(
                    progress,
                    entry,
                    file,
                    file_index,
                    files_total,
                    file.size_bytes,
                    DownloadState::Done,
                )
                .await;
                return Ok(());
            }
            tokio::fs::remove_file(&dest).await?;
        }

        match self
            .fetch_with_retry(entry, file, file_index, files_total, progress, cancel)
            .await
        {
            Ok(()) => {}
            Err(ModelManagerError::HashMismatch { .. }) => {
                self.fetch_with_retry(entry, file, file_index, files_total, progress, cancel)
                    .await?;
            }
            Err(e) => return Err(e),
        }

        let part = part_path(&dest);
        tokio::fs::rename(&part, &dest).await?;
        send_progress(
            progress,
            entry,
            file,
            file_index,
            files_total,
            file.size_bytes,
            DownloadState::Done,
        )
        .await;
        Ok(())
    }

    async fn fetch_with_retry(
        &self,
        entry: &CatalogEntry,
        file: &DownloadFile,
        file_index: usize,
        files_total: usize,
        progress: &mpsc::Sender<DownloadProgress>,
        cancel: &watch::Receiver<bool>,
    ) -> Result<(), ModelManagerError> {
        let mut attempt = 1;
        loop {
            match self
                .fetch_to_part(entry, file, file_index, files_total, progress, cancel)
                .await
            {
                Err(err) if err.is_transient() && attempt < MAX_DOWNLOAD_ATTEMPTS => {
                    let dest = self
                        .models_root
                        .join(install_rel_dir(entry))
                        .join(&file.dest_path);
                    let kept = tokio::fs::metadata(part_path(&dest))
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0);
                    send_progress(
                        progress,
                        entry,
                        file,
                        file_index,
                        files_total,
                        kept,
                        DownloadState::Reconnecting,
                    )
                    .await;
                    let delay = RETRY_BASE_DELAY * 2u32.pow(attempt - 1);
                    let mut cancel = cancel.clone();
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        changed = cancel.changed() => {
                            if changed.is_err() || *cancel.borrow() {
                                return Err(ModelManagerError::Cancelled);
                            }
                        }
                    }
                    if *cancel.borrow() {
                        return Err(ModelManagerError::Cancelled);
                    }
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn fetch_to_part(
        &self,
        entry: &CatalogEntry,
        file: &DownloadFile,
        file_index: usize,
        files_total: usize,
        progress: &mpsc::Sender<DownloadProgress>,
        cancel: &watch::Receiver<bool>,
    ) -> Result<(), ModelManagerError> {
        let mut cancel = cancel.clone();
        let install_dir = self.models_root.join(install_rel_dir(entry));
        let dest = install_dir.join(&file.dest_path);
        if !dest.starts_with(&self.models_root) {
            return Err(ModelManagerError::UnsafeDestPath(file.dest_path.clone()));
        }
        let part = part_path(&dest);

        let mut offset = tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if offset > file.size_bytes {
            tokio::fs::remove_file(&part).await?;
            offset = 0;
        }

        if offset == file.size_bytes {
            send_progress(
                progress,
                entry,
                file,
                file_index,
                files_total,
                0,
                DownloadState::Verifying,
            )
            .await;
            let mut hasher = Sha256::new();
            hash_into_with_progress(&part, &mut hasher, |bytes_done| {
                try_send_progress(
                    progress,
                    entry,
                    file,
                    file_index,
                    files_total,
                    bytes_done,
                    DownloadState::Verifying,
                );
                !*cancel.borrow()
            })
            .await?;
            let actual = hex::encode(hasher.finalize());
            if actual == file.sha256 {
                return Ok(());
            }
            tokio::fs::remove_file(&part).await?;
            return Err(ModelManagerError::HashMismatch {
                dest_path: file.dest_path.clone(),
                expected: file.sha256.clone(),
                actual,
            });
        }

        send_progress(
            progress,
            entry,
            file,
            file_index,
            files_total,
            offset,
            DownloadState::Downloading,
        )
        .await;

        let mut hasher = Sha256::new();
        if offset > 0 {
            hash_into(&part, &mut hasher).await?;
        }

        let mut request = self.client.get(&file.url);
        if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let response = request.send().await?;
        let status = response.status();
        if let Some(content_len) = response.content_length() {
            let start = if status == reqwest::StatusCode::PARTIAL_CONTENT {
                offset
            } else {
                0
            };
            if start + content_len > file.size_bytes {
                return Err(ModelManagerError::SizeMismatch {
                    dest_path: file.dest_path.clone(),
                    expected: file.size_bytes,
                    actual: start + content_len,
                });
            }
        }

        let mut out = if offset > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&part)
                .await?
        } else if status.is_success() {
            offset = 0;
            hasher = Sha256::new();
            tokio::fs::File::create(&part).await?
        } else {
            return Err(ModelManagerError::HttpStatus {
                status: status.as_u16(),
                url: file.url.clone(),
            });
        };

        let mut bytes_done = offset;
        let mut stream = response.bytes_stream();
        loop {
            let maybe_chunk = tokio::select! {
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        out.flush().await?;
                        return Err(ModelManagerError::Cancelled);
                    }
                    continue;
                }
                res = stream.next() => res,
            };

            let Some(chunk) = maybe_chunk else {
                break;
            };

            if *cancel.borrow() {
                out.flush().await?;
                return Err(ModelManagerError::Cancelled);
            }
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    out.flush().await?;
                    return Err(err.into());
                }
            };
            if bytes_done + (chunk.len() as u64) > file.size_bytes {
                drop(out);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(ModelManagerError::SizeMismatch {
                    dest_path: file.dest_path.clone(),
                    expected: file.size_bytes,
                    actual: bytes_done + (chunk.len() as u64),
                });
            }
            hasher.update(&chunk);
            out.write_all(&chunk).await?;
            bytes_done += chunk.len() as u64;
            send_progress(
                progress,
                entry,
                file,
                file_index,
                files_total,
                bytes_done,
                DownloadState::Downloading,
            )
            .await;
        }
        out.flush().await?;
        out.sync_all().await?;
        drop(out);

        if bytes_done != file.size_bytes {
            if bytes_done > file.size_bytes {
                let _ = tokio::fs::remove_file(&part).await;
            }
            return Err(ModelManagerError::SizeMismatch {
                dest_path: file.dest_path.clone(),
                expected: file.size_bytes,
                actual: bytes_done,
            });
        }

        send_progress(
            progress,
            entry,
            file,
            file_index,
            files_total,
            bytes_done,
            DownloadState::Verifying,
        )
        .await;

        let actual = hex::encode(hasher.finalize());
        if actual != file.sha256 {
            tokio::fs::remove_file(&part).await?;
            return Err(ModelManagerError::HashMismatch {
                dest_path: file.dest_path.clone(),
                expected: file.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(PART_SUFFIX);
    dest.with_file_name(name)
}

async fn hash_into(path: &Path, hasher: &mut Sha256) -> Result<(), ModelManagerError> {
    hash_into_with_progress(path, hasher, |_| true).await
}

const HASH_PROGRESS_STEP: u64 = 8 * 1024 * 1024;

async fn hash_into_with_progress<F>(
    path: &Path,
    hasher: &mut Sha256,
    mut on_progress: F,
) -> Result<(), ModelManagerError>
where
    F: FnMut(u64) -> bool,
{
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut bytes_done: u64 = 0;
    let mut next_report = HASH_PROGRESS_STEP;
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes_done += n as u64;
        if bytes_done >= next_report {
            if !on_progress(bytes_done) {
                return Err(ModelManagerError::Cancelled);
            }
            next_report = bytes_done + HASH_PROGRESS_STEP;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn send_progress(
    progress: &mpsc::Sender<DownloadProgress>,
    entry: &CatalogEntry,
    file: &DownloadFile,
    file_index: usize,
    files_total: usize,
    bytes_done: u64,
    state: DownloadState,
) {
    let _ = progress
        .send(DownloadProgress {
            model_id: entry.id.clone(),
            file_index,
            files_total,
            dest_path: file.dest_path.clone(),
            bytes_done,
            bytes_total: file.size_bytes,
            state,
        })
        .await;
}

fn try_send_progress(
    progress: &mpsc::Sender<DownloadProgress>,
    entry: &CatalogEntry,
    file: &DownloadFile,
    file_index: usize,
    files_total: usize,
    bytes_done: u64,
    state: DownloadState,
) {
    let _ = progress.try_send(DownloadProgress {
        model_id: entry.id.clone(),
        file_index,
        files_total,
        dest_path: file.dest_path.clone(),
        bytes_done,
        bytes_total: file.size_bytes,
        state,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dest: &str, sha: &str) -> DownloadFile {
        DownloadFile {
            url: format!("https://example.com/{dest}"),
            sha256: sha.to_string(),
            dest_path: dest.to_string(),
            size_bytes: 1,
        }
    }

    #[test]
    fn manifest_hash_is_order_independent() {
        let a = [file("a.bin", "11"), file("b.bin", "22")];
        let b = [file("b.bin", "22"), file("a.bin", "11")];
        assert_eq!(manifest_hash(&a), manifest_hash(&b));
    }

    #[test]
    fn manifest_hash_changes_with_content() {
        let a = [file("a.bin", "11")];
        let b = [file("a.bin", "33")];
        assert_ne!(manifest_hash(&a), manifest_hash(&b));
    }

    #[test]
    fn part_path_appends_suffix() {
        let p = part_path(Path::new("/x/models/llm/m1/model.gguf"));
        assert_eq!(p, Path::new("/x/models/llm/m1/model.gguf.part"));
    }

    #[test]
    fn missing_files_lists_only_the_projector_for_a_pre_projector_install() {
        let entry = crate::catalog::bundled_catalog()
            .models
            .iter()
            .find(|e| e.id == "qwen3.5-9b-instruct-q4km")
            .expect("entry");
        let dir = tempfile::tempdir().expect("tempdir");
        let weights = entry
            .download
            .files
            .iter()
            .find(|f| !f.dest_path.contains("mmproj"))
            .expect("weights");
        std::fs::write(dir.path().join(&weights.dest_path), b"stub").expect("write");

        let missing = missing_files(entry, dir.path());

        assert_eq!(missing, vec!["mmproj-F16.gguf".to_string()]);
    }

    #[test]
    fn missing_files_is_empty_when_every_catalog_file_is_present() {
        let entry = crate::catalog::bundled_catalog()
            .models
            .iter()
            .find(|e| e.id == "qwen3.5-9b-instruct-q4km")
            .expect("entry");
        let dir = tempfile::tempdir().expect("tempdir");
        for file in &entry.download.files {
            std::fs::write(dir.path().join(&file.dest_path), b"stub").expect("write");
        }
        assert!(missing_files(entry, dir.path()).is_empty());
    }

    #[test]
    fn task_dir_names_are_stable() {
        assert_eq!(task_dir_name(ModelTask::Llm), "llm");
        assert_eq!(task_dir_name(ModelTask::Tts), "tts");
        assert_eq!(task_dir_name(ModelTask::Unknown), "unknown");
    }
}
