use std::path::{Path, PathBuf};
use std::process::Stdio;

use rand::distr::{Alphanumeric, SampleString};
use tokio::process::{Child, Command};

const PACKAGED_BINARY: &str = if cfg!(windows) {
    "txtcaster-engine.exe"
} else {
    "txtcaster-engine"
};

const PACKAGED_DIR: &str = "txtcaster-engine";

pub const OFFLINE_ENV: [(&str, &str); 2] = [("HF_HUB_OFFLINE", "1"), ("TRANSFORMERS_OFFLINE", "1")];

pub fn generate_token() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 32)
}

#[derive(Debug, Clone)]
pub enum SidecarLaunch {
    Packaged { binary: PathBuf },
    Dev { engine_dir: PathBuf },
}

pub const DEV_ENGINE_ENV: &str = "TXTCASTER_DEV_ENGINE";

/// The only way a build outside `tauri dev` runs a Python checkout: an explicit absolute
/// path to its `engine/` directory. The working directory is never consulted, because a
/// folder a user happens to launch from must not be able to supply executable code.
pub fn explicit_dev_engine(raw: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let dir = PathBuf::from(raw?);
    if !dir.is_absolute() {
        tracing::warn!("{DEV_ENGINE_ENV} ignored: the path must be absolute");
        return None;
    }
    if !dir.join("pyproject.toml").is_file() || !venv_python(&dir).is_file() {
        tracing::warn!(
            "{DEV_ENGINE_ENV} ignored: {} has no pyproject.toml and .venv",
            dir.display()
        );
        return None;
    }
    Some(dir)
}

/// `tauri dev` keeps working out of the box: a debug build falls back to the checkout it
/// was compiled from. Release builds have no such fallback.
#[cfg(debug_assertions)]
pub fn compiled_checkout_engine() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3)?;
    let engine = root.join("engine");
    engine.join("pyproject.toml").is_file().then_some(engine)
}

#[cfg(not(debug_assertions))]
pub fn compiled_checkout_engine() -> Option<PathBuf> {
    None
}

pub fn resolve_launch(
    data_dir: Option<&Path>,
    exe_dir: &Path,
    resource_dir: Option<&Path>,
    checkout_engine: Option<&Path>,
) -> Option<SidecarLaunch> {
    let local_candidates = [
        data_dir.map(|dir| dir.join("engine").join(PACKAGED_BINARY)),
        data_dir.map(|dir| dir.join("engine").join(PACKAGED_DIR).join(PACKAGED_BINARY)),
        Some(exe_dir.join(PACKAGED_BINARY)),
        Some(exe_dir.join(PACKAGED_DIR).join(PACKAGED_BINARY)),
    ];
    for binary in local_candidates.into_iter().flatten() {
        if binary.is_file() {
            return Some(SidecarLaunch::Packaged { binary });
        }
    }
    if let Some(engine_dir) = checkout_engine {
        return Some(SidecarLaunch::Dev {
            engine_dir: engine_dir.to_path_buf(),
        });
    }
    let resource_candidates = [
        resource_dir.map(|dir| dir.join(PACKAGED_DIR).join(PACKAGED_BINARY)),
        resource_dir.map(|dir| dir.join(PACKAGED_BINARY)),
    ];
    for binary in resource_candidates.into_iter().flatten() {
        if binary.is_file() {
            return Some(SidecarLaunch::Packaged { binary });
        }
    }
    None
}

fn venv_python(engine_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        engine_dir.join(".venv").join("Scripts").join("python.exe")
    } else {
        engine_dir.join(".venv").join("bin").join("python")
    }
}

fn dev_python(engine_dir: &Path) -> PathBuf {
    let venv_python = venv_python(engine_dir);
    if venv_python.is_file() {
        venv_python
    } else {
        tracing::warn!(
            "venv python not found at {}; falling back to system python",
            venv_python.display()
        );
        PathBuf::from(if cfg!(windows) { "python" } else { "python3" })
    }
}

pub fn build_command(launch: &SidecarLaunch, token: &str) -> Command {
    let mut command = match launch {
        SidecarLaunch::Packaged { binary } => Command::new(binary),
        SidecarLaunch::Dev { engine_dir } => {
            let mut cmd = Command::new(dev_python(engine_dir));
            cmd.args(["-m", "txtcaster_engine"]).current_dir(engine_dir);
            cmd
        }
    };
    command
        .args(["--port", "0", "--watch-parent"])
        .env("TXTCASTER_TOKEN", token)
        .env("OMP_WAIT_POLICY", "passive")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in OFFLINE_ENV {
        command.env(key, value);
    }
    if std::env::var_os("APPIMAGE").is_some() {
        clear_appimage_runtime_env(&mut command);
    }
    crate::process_priority::lower(&mut command);
    crate::process_group::configure(&mut command);
    command
}

/// AppRun points these at the image's own libraries and Python for the app itself. The
/// engine brings its own, and a Python handed them cannot find its standard library.
fn clear_appimage_runtime_env(command: &mut Command) {
    for key in ["LD_LIBRARY_PATH", "PYTHONHOME", "PYTHONPATH"] {
        command.env_remove(key);
    }
}

pub fn spawn_sidecar(launch: &SidecarLaunch, token: &str) -> std::io::Result<Child> {
    let mut command = build_command(launch, token);
    let child = command.spawn()?;
    crate::process_group::assign(&child)?;
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_does_not_inherit_the_appimage_runtime() {
        let mut command = Command::new("engine");
        command.env("LD_LIBRARY_PATH", "/tmp/.mount/usr/lib");
        clear_appimage_runtime_env(&mut command);
        let cleared: Vec<_> = command
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        for key in ["LD_LIBRARY_PATH", "PYTHONHOME", "PYTHONPATH"] {
            assert!(
                cleared.iter().any(|name| name == key),
                "{key} must be cleared"
            );
        }
    }

    #[test]
    fn sidecar_does_not_leak_token_in_argv_and_sets_env() {
        let token = "test_secret_token_1234567890abcdef";
        let command = build_command(
            &SidecarLaunch::Packaged {
                binary: PathBuf::from("txtcaster-engine"),
            },
            token,
        );
        let args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|a| a == token),
            "token must not appear in argv: {args:?}"
        );
        let envs: Vec<(String, Option<String>)> = command
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|s| s.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(
            envs.contains(&("TXTCASTER_TOKEN".to_string(), Some(token.to_string()))),
            "TXTCASTER_TOKEN env var must be set: {envs:?}"
        );
    }

    #[test]
    fn token_is_32_alphanumeric_chars() {
        let token = generate_token();
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(token, generate_token());
    }

    #[test]
    fn sidecar_is_launched_with_hugging_face_offline() {
        let command = build_command(
            &SidecarLaunch::Packaged {
                binary: PathBuf::from("txtcaster-engine"),
            },
            "token",
        );
        let env: Vec<(String, Option<String>)> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for (key, value) in OFFLINE_ENV {
            assert!(
                env.contains(&(key.to_string(), Some(value.to_string()))),
                "sidecar env is missing {key}={value}: {env:?}"
            );
        }
    }

    #[test]
    fn resolve_prefers_packaged_binary() {
        let dir = tempdir();
        std::fs::write(dir.join(PACKAGED_BINARY), b"").unwrap();
        let launch = resolve_launch(None, &dir, None, None).unwrap();
        assert!(matches!(launch, SidecarLaunch::Packaged { .. }));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_finds_packaged_in_data_dir() {
        let data_dir = tempdir();
        let engine_dir = data_dir.join("engine");
        std::fs::create_dir_all(&engine_dir).unwrap();
        std::fs::write(engine_dir.join(PACKAGED_BINARY), b"").unwrap();
        let dummy_exe = tempdir();
        let launch = resolve_launch(Some(&data_dir), &dummy_exe, None, None).unwrap();
        assert!(
            matches!(launch, SidecarLaunch::Packaged { ref binary } if binary.starts_with(&engine_dir))
        );
        std::fs::remove_dir_all(&data_dir).unwrap();
        std::fs::remove_dir_all(&dummy_exe).unwrap();
    }

    #[test]
    fn resolve_finds_onedir_next_to_exe() {
        let dir = tempdir();
        let onedir = dir.join(PACKAGED_DIR);
        std::fs::create_dir_all(&onedir).unwrap();
        std::fs::write(onedir.join(PACKAGED_BINARY), b"").unwrap();
        let launch = resolve_launch(None, &dir, None, None).unwrap();
        assert!(
            matches!(launch, SidecarLaunch::Packaged { ref binary } if binary.starts_with(&onedir))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_finds_onedir_in_resource_dir() {
        let dir = tempdir();
        let resources = dir.join("resources");
        let onedir = resources.join(PACKAGED_DIR);
        std::fs::create_dir_all(&onedir).unwrap();
        std::fs::write(onedir.join(PACKAGED_BINARY), b"").unwrap();
        let launch = resolve_launch(None, &dir.join("bin"), Some(&resources), None).unwrap();
        assert!(
            matches!(launch, SidecarLaunch::Packaged { ref binary } if binary.starts_with(&onedir))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_uses_the_named_checkout_engine() {
        let dir = tempdir();
        let launch = resolve_launch(None, &dir.join("nope"), None, Some(&dir)).unwrap();
        assert!(matches!(launch, SidecarLaunch::Dev { engine_dir } if engine_dir == dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_returns_none_without_candidates() {
        let dir = tempdir();
        assert!(resolve_launch(None, &dir, None, None).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn checkout_with_venv(dir: &Path) -> PathBuf {
        let engine = dir.join("engine");
        let python = venv_python(&engine);
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, b"").unwrap();
        std::fs::write(engine.join("pyproject.toml"), b"").unwrap();
        engine
    }

    #[test]
    fn a_working_directory_holding_an_engine_is_not_a_launch_candidate() {
        let dir = tempdir();
        checkout_with_venv(&dir);
        assert!(resolve_launch(None, &dir.join("nope"), None, None).is_none());
        assert!(
            explicit_dev_engine(Some(std::ffi::OsStr::new("engine"))).is_none(),
            "a relative override resolves against the cwd"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_explicit_override_needs_an_absolute_checkout_with_its_venv() {
        let dir = tempdir();
        let engine = checkout_with_venv(&dir);
        assert_eq!(
            explicit_dev_engine(Some(engine.as_os_str())),
            Some(engine.clone())
        );
        std::fs::remove_file(venv_python(&engine)).unwrap();
        assert_eq!(explicit_dev_engine(Some(engine.as_os_str())), None);
        assert_eq!(explicit_dev_engine(None), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("txtcaster-test-{}", generate_token()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
