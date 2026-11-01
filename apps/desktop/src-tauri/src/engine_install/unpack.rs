use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::pack::ARCHIVE_ROOT;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnpackError {
    Io(String),
    Unsafe(String),
    TooLarge { cap: u64 },
    Cancelled,
}

impl std::fmt::Display for UnpackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnpackError::Io(why) => write!(f, "engine archive could not be read: {why}"),
            UnpackError::Unsafe(why) => write!(f, "engine archive refused: {why}"),
            UnpackError::TooLarge { cap } => {
                write!(f, "engine archive unpacks to more than {cap} bytes")
            }
            UnpackError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl From<io::Error> for UnpackError {
    fn from(error: io::Error) -> Self {
        UnpackError::Io(error.to_string())
    }
}

struct Parts {
    files: std::collections::VecDeque<PathBuf>,
    current: Option<File>,
}

impl Read for Parts {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.current.is_none() {
                match self.files.pop_front() {
                    Some(path) => self.current = Some(File::open(path)?),
                    None => return Ok(0),
                }
            }
            if let Some(file) = self.current.as_mut() {
                let n = file.read(buf)?;
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
            }
            self.current = None;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UnpackStats {
    pub files: u64,
    pub bytes: u64,
}

fn relative_under_root(raw: &Path) -> Result<PathBuf, UnpackError> {
    let mut components = raw.components();
    match components.next() {
        Some(Component::Normal(first)) if first == ARCHIVE_ROOT => {}
        _ => {
            return Err(UnpackError::Unsafe(format!(
                "entry {} is outside {ARCHIVE_ROOT}/",
                raw.display()
            )))
        }
    }
    let mut rel = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => rel.push(part),
            Component::CurDir => {}
            _ => {
                return Err(UnpackError::Unsafe(format!(
                    "entry {} leaves its folder",
                    raw.display()
                )))
            }
        }
    }
    Ok(rel)
}

/// Unpack the concatenated `parts` into `dest`, which must not exist yet. Only
/// directories and regular files under `txtcaster-engine/` are accepted; the total
/// of file bytes may not exceed `max_bytes`.
pub fn unpack_parts(
    parts: &[PathBuf],
    dest: &Path,
    max_bytes: u64,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(UnpackStats),
) -> Result<UnpackStats, UnpackError> {
    std::fs::create_dir(dest)?;
    let reader = Parts {
        files: parts.iter().cloned().collect(),
        current: None,
    };
    let decoder = zstd::stream::read::Decoder::new(reader)?;
    let mut archive = tar::Archive::new(decoder);
    let mut stats = UnpackStats::default();
    for entry in archive.entries()? {
        if cancel.load(Ordering::Relaxed) {
            return Err(UnpackError::Cancelled);
        }
        let mut entry = entry?;
        let raw = entry.path()?.into_owned();
        let rel = relative_under_root(&raw)?;
        let target = dest.join(&rel);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if !kind.is_file() {
            return Err(UnpackError::Unsafe(format!(
                "entry {} is a {kind:?}, only files and folders are allowed",
                raw.display()
            )));
        }
        if rel.as_os_str().is_empty() {
            return Err(UnpackError::Unsafe("a file named like the root".into()));
        }
        let size = entry.header().size()?;
        stats.bytes = stats
            .bytes
            .checked_add(size)
            .filter(|total| *total <= max_bytes)
            .ok_or(UnpackError::TooLarge { cap: max_bytes })?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = File::options().write(true).create_new(true).open(&target)?;
        let mut copied = 0u64;
        let mut buf = vec![0u8; 1 << 20];
        let mut body = (&mut entry).take(size);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(UnpackError::Cancelled);
            }
            let n = body.read(&mut buf)?;
            if n == 0 {
                break;
            }
            io::Write::write_all(&mut out, &buf[..n])?;
            copied += n as u64;
        }
        if copied != size {
            return Err(UnpackError::Io(format!("{} is truncated", raw.display())));
        }
        out.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if entry.header().mode()? & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))?;
        }
        stats.files += 1;
        on_progress(stats);
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_install::pack::pack_engine;

    fn engine_tree(root: &Path) {
        std::fs::create_dir_all(root.join("_internal/lib")).expect("dirs");
        std::fs::write(root.join("txtcaster-engine"), b"#!/bin/sh\necho engine\n").expect("bin");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join("txtcaster-engine"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("chmod");
        }
        std::fs::write(root.join("_internal/lib/data.bin"), vec![42u8; 5000]).expect("lib");
    }

    fn raw_archive(entries: &[(&[u8], tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (name, kind, body) in entries {
                let mut header = tar::Header::new_old();
                let field = &mut header.as_old_mut().name;
                field[..name.len()].copy_from_slice(name);
                header.set_entry_type(*kind);
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                if *kind == tar::EntryType::Symlink || *kind == tar::EntryType::Link {
                    header.set_link_name("txtcaster-engine/x").expect("link");
                    header.set_size(0);
                }
                header.set_cksum();
                let data: &[u8] = if kind.is_file() { body } else { &[] };
                builder.append(&header, data).expect("append");
            }
            builder.finish().expect("finish");
        }
        zstd::encode_all(tar_bytes.as_slice(), 1).expect("zstd")
    }

    fn unpack_bytes(
        bytes: Vec<u8>,
        max: u64,
    ) -> (tempfile::TempDir, Result<UnpackStats, UnpackError>) {
        let dir = tempfile::tempdir().expect("dir");
        let part = dir.path().join("e.tar.zst.000");
        std::fs::write(&part, bytes).expect("part");
        let dest = dir.path().join("engine.new");
        let result = unpack_parts(&[part], &dest, max, &AtomicBool::new(false), |_| {});
        (dir, result)
    }

    #[test]
    fn a_packed_engine_unpacks_to_the_same_files_and_keeps_the_executable_bit() {
        let src = tempfile::tempdir().expect("src");
        engine_tree(src.path());
        let out = tempfile::tempdir().expect("out");
        let packed = pack_engine(src.path(), &out.path().join("e"), 64, 1, 1).expect("pack");
        let paths: Vec<PathBuf> = packed.parts.iter().map(|p| p.path.clone()).collect();
        assert!(paths.len() > 1);
        let dest = out.path().join("engine.new");
        let mut seen = 0;
        let stats = unpack_parts(&paths, &dest, 1 << 20, &AtomicBool::new(false), |s| {
            seen = s.files
        })
        .expect("unpack");
        assert_eq!(stats.files, 2);
        assert_eq!(seen, 2);
        assert_eq!(stats.bytes, packed.unpacked_size);
        assert_eq!(
            std::fs::read(dest.join("_internal/lib/data.bin")).expect("lib"),
            vec![42u8; 5000]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dest.join("txtcaster-engine"))
                .expect("bin")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_sparse_file_in_the_onedir_installs() {
        use std::io::{Seek, SeekFrom, Write as _};
        let src = tempfile::tempdir().expect("src");
        engine_tree(src.path());
        let mut sparse = File::create(src.path().join("_internal/sparse.bin")).expect("sparse");
        sparse.seek(SeekFrom::Start(1 << 20)).expect("seek");
        sparse.write_all(b"end").expect("tail");
        drop(sparse);
        let out = tempfile::tempdir().expect("out");
        let packed = pack_engine(src.path(), &out.path().join("e"), 1 << 20, 1, 1).expect("pack");
        let parts: Vec<PathBuf> = packed.parts.iter().map(|p| p.path.clone()).collect();
        let dest = out.path().join("engine.new");
        unpack_parts(
            &parts,
            &dest,
            packed.unpacked_size,
            &AtomicBool::new(false),
            |_| {},
        )
        .expect("the installer accepts what the packer made");
        let body = std::fs::read(dest.join("_internal/sparse.bin")).expect("sparse back");
        assert_eq!(body.len(), (1 << 20) + 3);
        assert_eq!(&body[body.len() - 3..], b"end");
    }

    #[test]
    fn entries_that_escape_the_folder_are_refused() {
        for name in [
            &b"txtcaster-engine/../escaped"[..],
            b"/etc/escaped",
            b"other-root/file",
            b"../txtcaster-engine/file",
        ] {
            let (dir, result) = unpack_bytes(
                raw_archive(&[(name, tar::EntryType::Regular, b"x")]),
                1 << 20,
            );
            assert!(
                matches!(result, Err(UnpackError::Unsafe(_))),
                "{:?} → {result:?}",
                String::from_utf8_lossy(name)
            );
            assert!(!dir.path().join("escaped").exists());
        }
    }

    #[test]
    fn links_and_devices_are_refused() {
        for kind in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Char,
            tar::EntryType::Fifo,
        ] {
            let (_dir, result) =
                unpack_bytes(raw_archive(&[(b"txtcaster-engine/x", kind, b"")]), 1 << 20);
            assert!(
                matches!(result, Err(UnpackError::Unsafe(_))),
                "{kind:?} → {result:?}"
            );
        }
    }

    #[test]
    fn an_archive_bigger_than_its_declared_size_is_refused() {
        let (_dir, result) = unpack_bytes(
            raw_archive(&[
                (b"txtcaster-engine/a", tar::EntryType::Regular, &[1u8; 600]),
                (b"txtcaster-engine/b", tar::EntryType::Regular, &[2u8; 600]),
            ]),
            1000,
        );
        assert_eq!(result, Err(UnpackError::TooLarge { cap: 1000 }));
    }

    #[test]
    fn an_existing_destination_is_never_reused() {
        let dir = tempfile::tempdir().expect("dir");
        let part = dir.path().join("e.tar.zst.000");
        std::fs::write(
            &part,
            raw_archive(&[(b"txtcaster-engine/a", tar::EntryType::Regular, b"x")]),
        )
        .expect("part");
        let dest = dir.path().join("engine.new");
        std::fs::create_dir(&dest).expect("pre-existing");
        let result = unpack_parts(&[part], &dest, 1 << 20, &AtomicBool::new(false), |_| {});
        assert!(matches!(result, Err(UnpackError::Io(_))), "{result:?}");
    }

    #[test]
    fn a_cancelled_unpack_stops() {
        let dir = tempfile::tempdir().expect("dir");
        let part = dir.path().join("e.tar.zst.000");
        std::fs::write(
            &part,
            raw_archive(&[(b"txtcaster-engine/a", tar::EntryType::Regular, b"x")]),
        )
        .expect("part");
        let result = unpack_parts(
            &[part],
            &dir.path().join("engine.new"),
            1 << 20,
            &AtomicBool::new(true),
            |_| {},
        );
        assert_eq!(result, Err(UnpackError::Cancelled));
    }
}
