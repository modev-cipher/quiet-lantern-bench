//! libpulse 15.99 to 16.1 (Ubuntu 22.04, Debian 12) can stall the player after a seek; see
//! scripts/release/build_libpulse.sh. The deb and rpm carry 17.0 beside the app, and the app
//! restarts itself once with it first on the library path so WebKit's processes load it too.

use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

const SYSTEM_DIRS: [&str; 3] = [
    "/usr/lib/x86_64-linux-gnu/pulseaudio",
    "/usr/lib64/pulseaudio",
    "/usr/lib/pulseaudio",
];

pub fn restart_with_bundled_libpulse() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(path) = library_path(
        &exe,
        std::env::var_os("APPIMAGE").as_deref(),
        std::env::var_os("LD_LIBRARY_PATH").as_deref(),
        &SYSTEM_DIRS.map(Path::new),
    ) else {
        return;
    };
    let mut args = std::env::args_os();
    let mut command = std::process::Command::new(&exe);
    if let Some(arg0) = args.next() {
        command.arg0(arg0);
    }
    let error = command.args(args).env("LD_LIBRARY_PATH", path).exec();
    eprintln!("could not restart with the bundled libpulse: {error}");
}

pub fn bundled_libpulse_in_use() -> bool {
    let (Ok(exe), Some(current)) = (std::env::current_exe(), std::env::var_os("LD_LIBRARY_PATH"))
    else {
        return false;
    };
    bundled_dir(&exe).is_some_and(|dir| std::env::split_paths(&current).any(|entry| entry == dir))
}

fn bundled_dir(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?.parent()?.join("lib/txtCaster/pulse");
    dir.join("libpulse.so.0").is_file().then_some(dir)
}

fn library_path(
    exe: &Path,
    appimage: Option<&OsStr>,
    current: Option<&OsStr>,
    system_dirs: &[&Path],
) -> Option<OsString> {
    if appimage.is_some() {
        return None;
    }
    let bundled = bundled_dir(exe)?;
    let entries: Vec<PathBuf> = current
        .map(|value| {
            std::env::split_paths(value)
                .filter(|entry| !entry.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    if entries.contains(&bundled) {
        return None;
    }
    let affected = (15, 99)..(17, 0);
    if !affected.contains(&newest_system_libpulse(system_dirs)?) {
        return None;
    }
    std::env::join_paths(std::iter::once(bundled).chain(entries)).ok()
}

fn newest_system_libpulse(dirs: &[&Path]) -> Option<(u32, u32)> {
    dirs.iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|entry| common_library_version(&entry.ok()?.file_name().to_string_lossy()))
        .max()
}

fn common_library_version(file_name: &str) -> Option<(u32, u32)> {
    let version = file_name
        .strip_prefix("libpulsecommon-")?
        .strip_suffix(".so")?;
    let (major, minor) = version.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn install(root: &Path, bundled: bool) -> PathBuf {
        let exe = root.join("usr/bin/txtcaster");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        if bundled {
            let dir = root.join("usr/lib/txtCaster/pulse");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("libpulse.so.0"), "").unwrap();
        }
        exe
    }

    fn system(root: &Path, versions: &[&str]) -> PathBuf {
        let dir = root.join("system/pulseaudio");
        fs::create_dir_all(&dir).unwrap();
        for version in versions {
            fs::write(dir.join(format!("libpulsecommon-{version}.so")), "").unwrap();
        }
        dir
    }

    fn plan(
        exe: &Path,
        appimage: Option<&str>,
        current: Option<&str>,
        system: &Path,
    ) -> Option<OsString> {
        library_path(
            exe,
            appimage.map(OsStr::new),
            current.map(OsStr::new),
            &[system, Path::new("/nonexistent/pulseaudio")],
        )
    }

    #[test]
    fn ubuntu_22_04_and_debian_12_load_the_bundled_libpulse_first() {
        for version in ["15.99", "16.1"] {
            let root = tempfile::tempdir().unwrap();
            let exe = install(root.path(), true);
            let system = system(root.path(), &[version]);
            let bundled = root.path().join("usr/lib/txtCaster/pulse");
            assert_eq!(
                plan(&exe, None, None, &system),
                Some(bundled.clone().into_os_string()),
                "{version}"
            );
            let mut kept = bundled.into_os_string();
            kept.push(":/opt/gst");
            assert_eq!(plan(&exe, None, Some("/opt/gst"), &system), Some(kept));
        }
    }

    #[test]
    fn a_fixed_or_older_system_libpulse_is_left_alone() {
        for version in ["17.0", "17.1", "14.2"] {
            let root = tempfile::tempdir().unwrap();
            let exe = install(root.path(), true);
            let system = system(root.path(), &[version]);
            assert_eq!(plan(&exe, None, None, &system), None, "{version}");
        }
    }

    #[test]
    fn the_appimage_and_builds_without_the_bundled_copy_are_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let system = system(root.path(), &["15.99"]);
        let packaged = install(root.path(), true);
        assert_eq!(
            plan(&packaged, Some("/home/u/txtCaster.AppImage"), None, &system),
            None
        );

        let unbundled = tempfile::tempdir().unwrap();
        let dev = install(unbundled.path(), false);
        assert_eq!(plan(&dev, None, None, &system), None);
    }

    #[test]
    fn the_restarted_app_does_not_restart_again() {
        let root = tempfile::tempdir().unwrap();
        let exe = install(root.path(), true);
        let system = system(root.path(), &["15.99"]);
        let first = plan(&exe, None, Some("/opt/gst"), &system).unwrap();
        assert_eq!(plan(&exe, None, first.to_str(), &system), None);
    }

    #[test]
    fn without_a_system_libpulse_nothing_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let exe = install(root.path(), true);
        let system = system(root.path(), &[]);
        assert_eq!(plan(&exe, None, None, &system), None);
    }
}
