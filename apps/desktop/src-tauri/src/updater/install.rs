//! Hand a verified, still-open staged artifact to the platform. The core decides
//! the plan and where the staging file must live (same filesystem as what it
//! replaces); the process action is returned as a `Handoff` so the tauri layer
//! performs it and tests can observe it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::download::Staged;
use super::error::UpdateError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPlan {
    /// Windows: run the per-user installer passive + update mode, it relaunches us.
    WindowsInstaller,
    /// Linux running from an AppImage: replace the image file, then restart.
    ReplaceExecutable { target: PathBuf },
    /// Nothing we can safely replace (deb/rpm): the banner offers the release page.
    OpenReleasePage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handoff {
    /// The installer owns the rest; the app must exit now so files can be swapped.
    Exit,
    /// Files are in place; the app must restart into the new version.
    Restart,
}

/// Which artifact `kind` this platform wants, and what to do with it.
pub fn plan(kind: Option<&str>, appimage_env: Option<&str>) -> InstallPlan {
    if cfg!(target_os = "windows") {
        return if kind == Some("nsis") {
            InstallPlan::WindowsInstaller
        } else {
            InstallPlan::OpenReleasePage
        };
    }
    if cfg!(target_os = "linux") {
        if let (Some("appimage"), Some(path)) = (kind, appimage_env) {
            if !path.is_empty() {
                return InstallPlan::ReplaceExecutable {
                    target: PathBuf::from(path),
                };
            }
        }
    }
    InstallPlan::OpenReleasePage
}

/// Where the download must be written so that commit is a same-filesystem rename
/// (executable swap) or a run from a user-owned dir (installer).
pub fn staging_path(
    plan: &InstallPlan,
    downloads_dir: &Path,
    version: &str,
    platform: &str,
) -> Option<PathBuf> {
    match plan {
        InstallPlan::ReplaceExecutable { target } => {
            let mut name = target.file_name().unwrap_or_default().to_os_string();
            name.push(".new");
            Some(target.with_file_name(name))
        }
        InstallPlan::WindowsInstaller => {
            Some(downloads_dir.join(format!("txtcaster-{version}-{platform}-setup.exe")))
        }
        InstallPlan::OpenReleasePage => None,
    }
}

/// `/P` passive (progress UI, no wizard pages), `/R` relaunch on success,
/// `/UPDATE` keep shortcuts and app data, `/ARGS` = our own argv so the relaunch
/// resumes with the same arguments (the installer inherits our environment).
pub fn installer_args(current_args: &[OsString]) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["/P", "/R", "/UPDATE"].iter().map(OsString::from).collect();
    if !current_args.is_empty() {
        let mut joined = OsString::new();
        for (i, a) in current_args.iter().enumerate() {
            if i > 0 {
                joined.push(" ");
            }
            joined.push(quoted_for_command_line(a));
        }
        args.push(OsString::from("/ARGS"));
        args.push(joined);
    }
    args
}

/// `/ARGS` hands one flat string to the relaunched app, so an argument holding a space
/// has to stay one argument after CommandLineToArgvW splits it again.
fn quoted_for_command_line(arg: &OsString) -> OsString {
    let text = arg.to_string_lossy();
    if !text.contains([' ', '\t', '"']) {
        return arg.clone();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for c in text.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                continue;
            }
            '"' => quoted.push_str(&"\\".repeat(backslashes * 2 + 1)),
            _ => quoted.push_str(&"\\".repeat(backslashes)),
        }
        backslashes = 0;
        quoted.push(c);
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    OsString::from(quoted)
}

/// How long we watch the installer for an early, visible failure before exiting.
pub const INSTALLER_EARLY_FAILURE_WINDOW: Duration = Duration::from_secs(3);

pub fn perform(plan: &InstallPlan, staged: Staged) -> Result<Handoff, UpdateError> {
    match plan {
        InstallPlan::WindowsInstaller => {
            let path = staged.commit_path()?;
            let args: Vec<OsString> = std::env::args_os().skip(1).collect();
            spawn_installer(&path, &installer_args(&args)).map(|_| Handoff::Exit)
        }
        InstallPlan::ReplaceExecutable { target } => {
            staged.commit_rename(target).map(|_| Handoff::Restart)
        }
        InstallPlan::OpenReleasePage => {
            staged.discard();
            Err(UpdateError::Install(
                "this install cannot self-replace".to_string(),
            ))
        }
    }
}

/// Spawn, then watch briefly: an installer that exits non-zero within the window
/// (bad image, refused elevation, blocked by policy) is reported as a typed failure
/// instead of the app silently exiting into nothing. A still-running installer is
/// the success path: it finishes after we exit and relaunches us.
fn spawn_installer(installer: &Path, args: &[OsString]) -> Result<(), UpdateError> {
    let mut child = std::process::Command::new(installer)
        .args(args)
        .spawn()
        .map_err(|e| UpdateError::Install(format!("spawn installer: {e}")))?;
    let deadline = Instant::now() + INSTALLER_EARLY_FAILURE_WINDOW;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(UpdateError::Install(format!(
                    "installer exited early with {status}"
                )))
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(UpdateError::Install(format!("installer status: {e}"))),
        }
    }
    Ok(())
}
