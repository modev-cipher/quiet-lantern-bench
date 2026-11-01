//! Stream an artifact straight into its FINAL staging file (same filesystem as
//! the thing it will replace), hashing the bytes as they are written, verify the
//! signed digest, and hand back the still-open handle. Commit re-hashes through
//! that same handle and checks the path still names that inode before `rename`,
//! so the bytes that were verified are the bytes that get installed: there is no
//! second read from a path (review finding F1).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::error::UpdateError;
use super::manifest::{url_is_acceptable, Artifact};
use super::signing::PublicKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub received: u64,
    pub total: u64,
}

/// A verified artifact: open handle + the path it was created at + its digest.
pub struct Staged {
    file: std::fs::File,
    path: PathBuf,
    digest: [u8; 32],
}

/// Create the staging file exclusively. A pre-existing symlink is refused (typed),
/// a pre-existing regular file is unlinked, and `create_new` refuses anything that
/// appears in between (including a symlink, which O_EXCL never follows).
fn create_exclusive(path: &Path) -> Result<std::fs::File, UpdateError> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(UpdateError::StagingSymlink(path.display().to_string()));
        }
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o700);
    }
    opts.open(path).map_err(UpdateError::from)
}

/// Windows file identity for the staged-file recheck: volume serial number +
/// file id, which "uniquely identify a file on a single computer"
/// (FILE_ID_INFO, https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info,
/// checked 2026-10-01). The 128-bit id comes from
/// `GetFileInformationByHandleEx(FileIdInfo)`
/// (https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getfileinformationbyhandleex,
/// checked 2026-10-01); a filesystem that cannot answer that class falls back
/// to `GetFileInformationByHandle`'s 64-bit index. Any query failure refuses
/// the install — the check fails closed.
#[cfg(windows)]
mod windows_identity {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_128, FILE_ID_INFO,
    };

    #[derive(Debug, PartialEq, Eq)]
    pub(super) struct FileIdentity {
        volume: u64,
        id: [u8; 16],
    }

    pub(super) fn of_handle(file: &std::fs::File) -> std::io::Result<FileIdentity> {
        let handle = file.as_raw_handle() as HANDLE;
        let mut info = FILE_ID_INFO {
            VolumeSerialNumber: 0,
            FileId: FILE_ID_128::default(),
        };
        // SAFETY: `handle` is owned by `file`, which outlives this call; the
        // buffer is a FILE_ID_INFO and its exact size is passed.
        let answered = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                (&mut info as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if answered != 0 {
            return Ok(FileIdentity {
                volume: info.VolumeSerialNumber,
                id: info.FileId.Identifier,
            });
        }
        // SAFETY: BY_HANDLE_FILE_INFORMATION is plain integers and FILETIMEs,
        // for which all-zero is a valid value.
        let mut basic: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: as above — a live handle and a correctly typed out-pointer.
        if unsafe { GetFileInformationByHandle(handle, &mut basic) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut id = [0u8; 16];
        id[..4].copy_from_slice(&basic.nFileIndexLow.to_le_bytes());
        id[4..8].copy_from_slice(&basic.nFileIndexHigh.to_le_bytes());
        Ok(FileIdentity {
            volume: u64::from(basic.dwVolumeSerialNumber),
            id,
        })
    }

    /// The identity of whatever the path names NOW, without following a
    /// reparse point (FILE_FLAG_OPEN_REPARSE_POINT: "CreateFile will attempt
    /// to open the reparse point",
    /// https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew,
    /// checked 2026-10-01) and with no data access — "the application can
    /// query certain metadata … without accessing that file".
    pub(super) fn of_path_unfollowed(path: &Path) -> std::io::Result<FileIdentity> {
        let file = std::fs::OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        of_handle(&file)
    }
}

fn parse_digest(hex_digest: &str) -> Result<[u8; 32], UpdateError> {
    hex::decode(hex_digest.trim())
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| UpdateError::ManifestMalformed("sha256 is not 32 hex bytes".into()))
}

pub async fn stage_download(
    client: &reqwest::Client,
    artifact: &Artifact,
    staging_path: &Path,
    key: &PublicKey,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(Progress),
) -> Result<Staged, UpdateError> {
    let url = reqwest::Url::parse(&artifact.url)
        .map_err(|_| UpdateError::InsecureUrl(artifact.url.clone()))?;
    if !url_is_acceptable(&url) {
        return Err(UpdateError::InsecureUrl(artifact.url.clone()));
    }
    // `sha256` comes from the manifest that was parsed only AFTER its signature
    // verified, and the artifact signature below is over the digest we compute.
    let expected = parse_digest(&artifact.sha256)?;
    let std_file = create_exclusive(staging_path)?;
    let result = stream_into(
        client,
        &url,
        artifact,
        std_file.try_clone()?,
        cancel,
        &mut on_progress,
    )
    .await;
    match result {
        Ok(actual) if actual != expected => {
            let _ = std::fs::remove_file(staging_path);
            Err(UpdateError::Digest {
                expected: artifact.sha256.to_lowercase(),
                actual: hex::encode(actual),
            })
        }
        Ok(actual) => match key.verify_artifact_digest(&actual, &artifact.signature) {
            Ok(()) => Ok(Staged {
                file: std_file,
                path: staging_path.to_path_buf(),
                digest: actual,
            }),
            Err(e) => {
                let _ = std::fs::remove_file(staging_path);
                Err(e)
            }
        },
        Err(e) => {
            let _ = std::fs::remove_file(staging_path);
            Err(e)
        }
    }
}

async fn stream_into(
    client: &reqwest::Client,
    url: &reqwest::Url,
    artifact: &Artifact,
    file: std::fs::File,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(Progress),
) -> Result<[u8; 32], UpdateError> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| UpdateError::Install(format!("download request failed: {e}")))?
        .error_for_status()
        .map_err(|e| UpdateError::Install(format!("download failed with status: {e}")))?;
    if response
        .content_length()
        .is_some_and(|len| len > artifact.size)
    {
        return Err(UpdateError::SizeExceeded { cap: artifact.size });
    }
    let mut file = tokio::fs::File::from_std(file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::SeqCst) {
            return Err(UpdateError::Cancelled);
        }
        let chunk =
            chunk.map_err(|e| UpdateError::Install(format!("download stream interrupted: {e}")))?;
        received += chunk.len() as u64;
        if received > artifact.size {
            return Err(UpdateError::SizeExceeded { cap: artifact.size });
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        on_progress(Progress {
            received,
            total: artifact.size,
        });
    }
    file.flush().await?;
    file.sync_all().await?;
    if received != artifact.size {
        return Err(UpdateError::SizeMismatch {
            expected: artifact.size,
            actual: received,
        });
    }
    Ok(hasher.finalize().into())
}

impl Staged {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Re-hash through our own handle (catches contents changed via another
    /// handle) and check the path still names this inode (catches a file renamed
    /// over the staging path after verification). Both are typed refusals.
    fn recheck(&mut self) -> Result<(), UpdateError> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = self.file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let now: [u8; 32] = hasher.finalize().into();
        if now != self.digest {
            return Err(UpdateError::StagedTampered);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let by_fd = self.file.metadata()?;
            let by_path = std::fs::symlink_metadata(&self.path)?;
            if by_path.file_type().is_symlink()
                || by_path.ino() != by_fd.ino()
                || by_path.dev() != by_fd.dev()
            {
                return Err(UpdateError::StagedSubstituted);
            }
        }
        #[cfg(windows)]
        {
            let by_path = std::fs::symlink_metadata(&self.path)?;
            if by_path.file_type().is_symlink()
                || windows_identity::of_handle(&self.file)?
                    != windows_identity::of_path_unfollowed(&self.path)?
            {
                return Err(UpdateError::StagedSubstituted);
            }
        }
        Ok(())
    }

    /// Atomically replace `target` (same filesystem: the staging file was created
    /// beside it). The old file is intact until the rename succeeds.
    pub fn commit_rename(mut self, target: &Path) -> Result<(), UpdateError> {
        if let Err(e) = self.recheck() {
            let _ = std::fs::remove_file(&self.path);
            return Err(e);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = self
                .file
                .set_permissions(std::fs::Permissions::from_mode(0o755))
            {
                let _ = std::fs::remove_file(&self.path);
                return Err(e.into());
            }
        }
        std::fs::rename(&self.path, target).map_err(|e| {
            let _ = std::fs::remove_file(&self.path);
            UpdateError::Install(format!("replace {}: {e}", target.display()))
        })
    }

    /// For an installer we must run from a path: recheck, then yield the path.
    pub fn commit_path(mut self) -> Result<PathBuf, UpdateError> {
        if let Err(e) = self.recheck() {
            let _ = std::fs::remove_file(&self.path);
            return Err(e);
        }
        Ok(self.path.clone())
    }

    pub fn discard(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Remove leftovers of an interrupted earlier run in the downloads dir.
pub async fn sweep_staging(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let _ = tokio::fs::remove_file(entry.path()).await;
    }
}
