use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use tokio::process::Command;

#[derive(Debug)]
pub enum FfmpegError {
    NotFound,
    Spawn(String),
    Wait(String),
}

impl std::fmt::Display for FfmpegError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfmpegError::NotFound => write!(
                f,
                "ffmpeg could not be started: no bundled copy and none on PATH \
                 (set TXTCASTER_FFMPEG to override)"
            ),
            FfmpegError::Spawn(msg) => write!(f, "failed to spawn ffmpeg: {msg}"),
            FfmpegError::Wait(msg) => write!(f, "ffmpeg failed: {msg}"),
        }
    }
}

const BUNDLED_DIR: &str = "ffmpeg";

const BUNDLED_BINARY: &str = if cfg!(windows) {
    "ffmpeg.exe"
} else {
    "ffmpeg"
};

const BUNDLED_PROBE_BINARY: &str = if cfg!(windows) {
    "ffprobe.exe"
} else {
    "ffprobe"
};

const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(30);

static BUNDLED: OnceLock<PathBuf> = OnceLock::new();

static BUNDLED_PROBE: OnceLock<PathBuf> = OnceLock::new();

static AVAILABLE: OnceLock<()> = OnceLock::new();

pub fn bundled_candidate(resource_dir: &Path) -> PathBuf {
    resource_dir.join(BUNDLED_DIR).join(BUNDLED_BINARY)
}

pub fn bundled_probe_candidate(resource_dir: &Path) -> PathBuf {
    resource_dir.join(BUNDLED_DIR).join(BUNDLED_PROBE_BINARY)
}

fn find_bundled(resource_dir: Option<&Path>, candidate: fn(&Path) -> PathBuf) -> Option<PathBuf> {
    resource_dir.map(candidate).filter(|path| path.is_file())
}

pub fn register_bundled(resource_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(probe) = find_bundled(resource_dir, bundled_probe_candidate) {
        BUNDLED_PROBE.get_or_init(|| probe);
    }
    let candidate = find_bundled(resource_dir, bundled_candidate)?;
    Some(BUNDLED.get_or_init(|| candidate).clone())
}

fn choose(env_override: Option<OsString>, bundled: Option<&Path>, fallback: &str) -> OsString {
    match (env_override, bundled) {
        (Some(path), _) if !path.is_empty() => path,
        (_, Some(path)) => path.as_os_str().to_owned(),
        _ => OsString::from(fallback),
    }
}

pub fn binary() -> OsString {
    choose(
        std::env::var_os("TXTCASTER_FFMPEG"),
        BUNDLED.get().map(PathBuf::as_path),
        "ffmpeg",
    )
}

pub fn probe_binary() -> OsString {
    choose(
        std::env::var_os("TXTCASTER_FFPROBE"),
        BUNDLED_PROBE.get().map(PathBuf::as_path),
        "ffprobe",
    )
}

/// linuxdeploy moves FFmpeg's libraries to the AppImage's `usr/lib`, two levels above the
/// binaries, and writes a search path that does not reach them.
fn appimage_library_dir(binary: &Path, appdir: Option<&OsStr>) -> Option<PathBuf> {
    let usr_lib = Path::new(appdir?).join("usr").join("lib");
    binary
        .starts_with(&usr_lib)
        .then(|| binary.ancestors().nth(3).map(Path::to_path_buf))
        .flatten()
}

fn point_at_bundled_libraries(command: &mut Command, binary: &OsStr) {
    if let Some(dir) =
        appimage_library_dir(Path::new(binary), std::env::var_os("APPDIR").as_deref())
    {
        command.env("LD_LIBRARY_PATH", dir);
    }
}

pub fn probe_command() -> Command {
    let binary = probe_binary();
    let mut command = Command::new(&binary);
    point_at_bundled_libraries(&mut command, &binary);
    command.stdin(Stdio::null()).kill_on_drop(true);
    crate::process_priority::lower(&mut command);
    command
}

pub async fn ensure_available() -> Result<(), FfmpegError> {
    if AVAILABLE.get().is_some() {
        return Ok(());
    }
    tokio::time::timeout(PREFLIGHT_TIMEOUT, preflight())
        .await
        .map_err(|_| {
            FfmpegError::Wait(format!(
                "no answer to -version within {}s",
                PREFLIGHT_TIMEOUT.as_secs()
            ))
        })??;
    let _ = AVAILABLE.set(());
    Ok(())
}

async fn preflight() -> Result<(), FfmpegError> {
    let output = run(&["-hide_banner", "-version"]).await?;
    if !output.status.success() {
        return Err(FfmpegError::Wait(stderr_tail(&output)));
    }
    let probe = probe_command()
        .args(["-hide_banner", "-version"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                FfmpegError::Spawn("ffprobe could not be started: no bundled copy and none on PATH (set TXTCASTER_FFPROBE to override)".into())
            } else {
                FfmpegError::Spawn(format!("failed to spawn ffprobe: {err}"))
            }
        })?;
    if !probe.status.success() {
        return Err(FfmpegError::Wait(format!(
            "ffprobe: {}",
            stderr_tail(&probe)
        )));
    }
    Ok(())
}

pub async fn spawn(args: &[&str]) -> Result<crate::process_group::OwnedChild, FfmpegError> {
    let binary = binary();
    let mut command = Command::new(&binary);
    point_at_bundled_libraries(&mut command, &binary);
    if !args.contains(&"-protocol_whitelist") {
        command.args(["-protocol_whitelist", "file,pipe,concat"]);
    }
    command.args(args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::process_priority::lower(&mut command);
    crate::process_group::configure(&mut command);
    let child = command.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            FfmpegError::NotFound
        } else {
            FfmpegError::Spawn(err.to_string())
        }
    })?;
    crate::process_group::assign(&child).map_err(|err| FfmpegError::Spawn(err.to_string()))?;
    Ok(crate::process_group::OwnedChild::new(child))
}

pub async fn run(args: &[&str]) -> Result<Output, FfmpegError> {
    let mut child = spawn(args).await?;
    child
        .wait_with_output()
        .await
        .map_err(|err| FfmpegError::Wait(err.to_string()))
}

pub fn stderr_tail(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = text.lines().rev().take(6).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_binary_inside_the_appimage_gets_its_library_dir() {
        let appdir = OsStr::new("/tmp/.mount_x");
        assert_eq!(
            appimage_library_dir(
                Path::new("/tmp/.mount_x/usr/lib/txtCaster/ffmpeg/ffmpeg"),
                Some(appdir)
            ),
            Some(PathBuf::from("/tmp/.mount_x/usr/lib"))
        );
        assert_eq!(
            appimage_library_dir(Path::new("/usr/bin/ffmpeg"), Some(appdir)),
            None
        );
        assert_eq!(
            appimage_library_dir(Path::new("/usr/lib/txtCaster/ffmpeg/ffmpeg"), None),
            None
        );
    }

    use super::*;

    #[test]
    fn an_explicit_override_beats_the_bundled_binary() {
        let bundled = Path::new("/opt/app/ffmpeg/ffmpeg");
        assert_eq!(
            choose(
                Some(OsString::from("/custom/ffmpeg")),
                Some(bundled),
                "ffmpeg"
            ),
            OsString::from("/custom/ffmpeg")
        );
    }

    #[test]
    fn the_bundled_binary_beats_path_lookup() {
        let bundled = Path::new("/opt/app/ffmpeg/ffmpeg");
        assert_eq!(choose(None, Some(bundled), "ffmpeg"), bundled.as_os_str());
        assert_eq!(
            choose(Some(OsString::new()), Some(bundled), "ffmpeg"),
            bundled.as_os_str(),
            "an empty override is unset, not a binary named \"\""
        );
    }

    #[test]
    fn path_lookup_is_the_last_resort() {
        assert_eq!(choose(None, None, "ffmpeg"), OsString::from("ffmpeg"));
        assert_eq!(choose(None, None, "ffprobe"), OsString::from("ffprobe"));
    }

    #[test]
    fn the_bundled_binary_lives_in_its_own_resource_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(find_bundled(Some(dir.path()), bundled_candidate), None);
        assert_eq!(find_bundled(None, bundled_candidate), None);

        let candidate = bundled_candidate(dir.path());
        assert_eq!(candidate, dir.path().join("ffmpeg").join(BUNDLED_BINARY));
        std::fs::create_dir_all(candidate.parent().expect("parent")).expect("mkdir");
        std::fs::write(&candidate, b"").expect("write");
        assert_eq!(
            find_bundled(Some(dir.path()), bundled_candidate),
            Some(candidate)
        );
    }

    #[test]
    fn the_bundled_prober_sits_beside_the_bundled_encoder() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            find_bundled(Some(dir.path()), bundled_probe_candidate),
            None
        );

        let probe = bundled_probe_candidate(dir.path());
        assert_eq!(probe, dir.path().join("ffmpeg").join(BUNDLED_PROBE_BINARY));
        assert_eq!(probe.parent(), bundled_candidate(dir.path()).parent());
        std::fs::create_dir_all(probe.parent().expect("parent")).expect("mkdir");
        std::fs::write(&probe, b"").expect("write");
        assert_eq!(
            find_bundled(Some(dir.path()), bundled_probe_candidate),
            Some(probe)
        );
    }
}
