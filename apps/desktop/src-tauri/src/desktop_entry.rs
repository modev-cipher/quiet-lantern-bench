//! An AppImage has no installer, so on start it gives itself a menu entry and icon in the
//! user's data directory (the deb and rpm install theirs system-wide).

use std::io;
use std::path::{Path, PathBuf};

pub const DESKTOP_FILE: &str = "io.github.meedoomostafa.txtcaster.desktop";
const ICON_NAME: &str = "txtcaster";
const ICONS: [(&str, &[u8]); 4] = [
    ("32x32", include_bytes!("../icons/32x32.png")),
    ("128x128", include_bytes!("../icons/128x128.png")),
    ("256x256", include_bytes!("../icons/128x128@2x.png")),
    ("512x512", include_bytes!("../icons/icon.png")),
];

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Written,
    Unchanged,
    ManagedElsewhere,
    RemovedByUser,
    UnsupportedPath,
}

pub fn integrate_running_appimage(data_dir: &Path) {
    let Some(appimage) = std::env::var_os("APPIMAGE").filter(|path| !path.is_empty()) else {
        return;
    };
    let xdg = std::env::var("XDG_DATA_HOME").ok();
    let home = std::env::var("HOME").ok();
    let Some(data_home) = data_home(xdg.as_deref(), home.as_deref()) else {
        tracing::warn!("menu entry skipped: no data directory");
        return;
    };
    match integrate(
        Path::new(&appimage),
        &data_home,
        &data_dir.join("desktop-entry"),
    ) {
        Ok(Outcome::Unchanged) => {}
        Ok(outcome) => tracing::info!(
            "menu entry for {}: {outcome:?}",
            Path::new(&appimage).display()
        ),
        Err(err) => tracing::warn!("menu entry not written: {err}"),
    }
}

pub fn remove_running_appimage_entry() {
    if std::env::var_os("APPIMAGE").is_none() {
        return;
    }
    let xdg = std::env::var("XDG_DATA_HOME").ok();
    let home = std::env::var("HOME").ok();
    if let Some(data_home) = data_home(xdg.as_deref(), home.as_deref()) {
        if let Err(err) = remove(&data_home) {
            tracing::warn!("menu entry not removed: {err}");
        }
    }
}

pub fn integrate(appimage: &Path, data_home: &Path, marker: &Path) -> io::Result<Outcome> {
    // GLib looks for the Exec program before it expands %%, so a path with % never shows.
    let Some(path) = appimage
        .to_str()
        .filter(|path| !path.chars().any(|c| c.is_control() || c == '%'))
    else {
        return Ok(Outcome::UnsupportedPath);
    };
    let applications = data_home.join("applications");
    let entry_path = applications.join(DESKTOP_FILE);
    if managed_elsewhere(&applications, path)? {
        return Ok(Outcome::ManagedElsewhere);
    }
    if marker.exists() && !entry_path.exists() {
        return Ok(Outcome::RemovedByUser);
    }
    let mut changed = false;
    for (size, bytes) in ICONS {
        changed |= write_if_changed(&icon_path(data_home, size), bytes)?;
    }
    changed |= write_if_changed(&entry_path, desktop_entry(path).as_bytes())?;
    if !marker.exists() {
        write_if_changed(marker, b"")?;
    }
    Ok(if changed {
        Outcome::Written
    } else {
        Outcome::Unchanged
    })
}

pub fn remove(data_home: &Path) -> io::Result<()> {
    let files = ICONS
        .iter()
        .map(|(size, _)| icon_path(data_home, size))
        .chain([data_home.join("applications").join(DESKTOP_FILE)]);
    for file in files {
        match std::fs::remove_file(&file) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
    }
    Ok(())
}

pub fn data_home(xdg_data_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = xdg_data_home.map(Path::new).filter(|dir| dir.is_absolute()) {
        return Some(dir.to_path_buf());
    }
    let home = Path::new(home?);
    home.is_absolute().then(|| home.join(".local/share"))
}

fn icon_path(data_home: &Path, size: &str) -> PathBuf {
    data_home.join(format!("icons/hicolor/{size}/apps/{ICON_NAME}.png"))
}

fn desktop_entry(appimage: &str) -> String {
    let mut quoted = String::from("\"");
    for c in appimage.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    format!(
        "[Desktop Entry]\nType=Application\nName=txtCaster\n\
         Comment=Local-first offline book-to-audio/video converter\n\
         Exec={}\nTryExec={}\nIcon={ICON_NAME}\nTerminal=false\nCategories=Office;\n\
         StartupWMClass=txtcaster\n",
        escape_value(&quoted),
        escape_value(appimage),
    )
}

fn escape_value(value: &str) -> String {
    value.replace('\\', "\\\\")
}

fn managed_elsewhere(applications: &Path, appimage: &str) -> io::Result<bool> {
    let entries = match std::fs::read_dir(applications) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().map_or(true, |ext| ext != "desktop")
            || path.file_name().is_some_and(|name| name == DESKTOP_FILE)
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let runs_it = text.lines().any(|line| {
            (line.starts_with("Exec=") || line.starts_with("TryExec=")) && line.contains(appimage)
        });
        if runs_it {
            return Ok(true);
        }
    }
    Ok(false)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if std::fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(false);
    }
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    io::Write::write_all(&mut staged, bytes)?;
    staged.persist(path).map_err(|err| err.error)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        data_home: PathBuf,
        marker: PathBuf,
        appimage: PathBuf,
    }

    fn fixture(appimage_name: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let data_home = dir.path().join("share");
        let marker = dir.path().join("data").join("desktop-entry");
        let appimage = dir.path().join("Apps").join(appimage_name);
        std::fs::create_dir_all(appimage.parent().unwrap()).unwrap();
        std::fs::write(&appimage, b"appimage").unwrap();
        Fixture {
            _dir: dir,
            data_home,
            marker,
            appimage,
        }
    }

    fn entry_path(f: &Fixture) -> PathBuf {
        f.data_home.join("applications").join(DESKTOP_FILE)
    }

    fn entry(f: &Fixture) -> String {
        std::fs::read_to_string(entry_path(f)).unwrap()
    }

    fn key<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
        entry
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
    }

    #[test]
    fn first_start_adds_a_menu_entry_that_runs_this_appimage_with_its_icon() {
        let f = fixture("txtCaster-amd64.AppImage");
        assert_eq!(
            integrate(&f.appimage, &f.data_home, &f.marker).unwrap(),
            Outcome::Written
        );
        let text = entry(&f);
        assert!(text.starts_with("[Desktop Entry]\n"));
        let path = f.appimage.to_str().unwrap();
        assert_eq!(key(&text, "Exec"), Some(format!("\"{path}\"").as_str()));
        assert_eq!(key(&text, "TryExec"), Some(path));
        assert_eq!(key(&text, "Icon"), Some(ICON_NAME));
        assert_eq!(key(&text, "Type"), Some("Application"));
        assert_eq!(key(&text, "Name"), Some("txtCaster"));
        assert_eq!(key(&text, "StartupWMClass"), Some("txtcaster"));
        for (size, bytes) in ICONS {
            let icon = f
                .data_home
                .join(format!("icons/hicolor/{size}/apps/{ICON_NAME}.png"));
            assert_eq!(std::fs::read(icon).unwrap(), bytes, "{size}");
        }
        assert!(f.marker.is_file());
    }

    #[test]
    fn a_second_start_from_the_same_place_changes_nothing() {
        let f = fixture("txtCaster.AppImage");
        integrate(&f.appimage, &f.data_home, &f.marker).unwrap();
        assert_eq!(
            integrate(&f.appimage, &f.data_home, &f.marker).unwrap(),
            Outcome::Unchanged
        );
    }

    #[test]
    fn a_moved_appimage_moves_its_menu_entry_with_it() {
        let f = fixture("txtCaster.AppImage");
        integrate(&f.appimage, &f.data_home, &f.marker).unwrap();
        let moved = f.appimage.with_file_name("renamed.AppImage");
        std::fs::rename(&f.appimage, &moved).unwrap();
        assert_eq!(
            integrate(&moved, &f.data_home, &f.marker).unwrap(),
            Outcome::Written
        );
        assert_eq!(key(&entry(&f), "TryExec"), Some(moved.to_str().unwrap()));
    }

    #[test]
    fn paths_with_spaces_quotes_and_shell_characters_stay_one_argument() {
        let f = fixture("my \"txt\" $Caster `x`\\.AppImage");
        integrate(&f.appimage, &f.data_home, &f.marker).unwrap();
        let dir = f.appimage.parent().unwrap().to_str().unwrap();
        let text = entry(&f);
        assert_eq!(
            key(&text, "Exec").unwrap(),
            format!("\"{dir}/my \\\\\"txt\\\\\" \\\\$Caster \\\\`x\\\\`\\\\\\\\.AppImage\"")
        );
        assert_eq!(
            key(&text, "TryExec").unwrap(),
            format!("{dir}/my \"txt\" $Caster `x`\\\\.AppImage")
        );
    }

    #[test]
    fn a_path_with_a_line_break_or_percent_sign_gets_no_entry() {
        for name in ["two\nlines.AppImage", "100%.AppImage"] {
            let f = fixture(name);
            assert_eq!(
                integrate(&f.appimage, &f.data_home, &f.marker).unwrap(),
                Outcome::UnsupportedPath
            );
            assert!(!entry_path(&f).exists());
        }
    }

    #[test]
    fn an_entry_another_tool_made_for_this_appimage_is_left_alone() {
        let f = fixture("txtCaster.AppImage");
        let apps = f.data_home.join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("appimagekit_0123-txtCaster.desktop"),
            format!("[Desktop Entry]\nExec={} %U\n", f.appimage.display()),
        )
        .unwrap();
        assert_eq!(
            integrate(&f.appimage, &f.data_home, &f.marker).unwrap(),
            Outcome::ManagedElsewhere
        );
        assert!(!entry_path(&f).exists());
    }

    #[test]
    fn an_entry_the_user_deleted_stays_deleted() {
        let f = fixture("txtCaster.AppImage");
        integrate(&f.appimage, &f.data_home, &f.marker).unwrap();
        std::fs::remove_file(entry_path(&f)).unwrap();
        assert_eq!(
            integrate(&f.appimage, &f.data_home, &f.marker).unwrap(),
            Outcome::RemovedByUser
        );
        assert!(!entry_path(&f).exists());
    }

    #[test]
    fn remove_takes_away_the_entry_and_icons_and_tolerates_their_absence() {
        let f = fixture("txtCaster.AppImage");
        integrate(&f.appimage, &f.data_home, &f.marker).unwrap();
        remove(&f.data_home).unwrap();
        assert!(!entry_path(&f).exists());
        for (size, _) in ICONS {
            assert!(!f
                .data_home
                .join(format!("icons/hicolor/{size}/apps/{ICON_NAME}.png"))
                .exists());
        }
        remove(&f.data_home).unwrap();
    }

    #[test]
    fn data_home_follows_the_xdg_base_directory_rules() {
        assert_eq!(
            data_home(Some("/x/share"), Some("/home/u")),
            Some(PathBuf::from("/x/share"))
        );
        assert_eq!(
            data_home(Some("relative"), Some("/home/u")),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(
            data_home(Some(""), Some("/home/u")),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(
            data_home(None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(data_home(None, None), None);
        assert_eq!(data_home(None, Some("relative")), None);
    }
}
