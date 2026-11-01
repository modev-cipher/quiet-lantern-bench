use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const ARCHIVE_ROOT: &str = "txtcaster-engine";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedPart {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packed {
    pub parts: Vec<PackedPart>,
    pub size: u64,
    pub sha256: [u8; 32],
    pub unpacked_size: u64,
}

pub fn part_path(prefix: &Path, index: usize) -> PathBuf {
    let mut name = prefix.as_os_str().to_owned();
    name.push(format!(".tar.zst.{index:03}"));
    PathBuf::from(name)
}

struct SplitWriter {
    prefix: PathBuf,
    part_size: u64,
    current: Option<(BufWriter<File>, Sha256, u64)>,
    done: Vec<PackedPart>,
    whole: Sha256,
    total: u64,
}

impl SplitWriter {
    fn new(prefix: &Path, part_size: u64) -> Self {
        SplitWriter {
            prefix: prefix.to_path_buf(),
            part_size,
            current: None,
            done: Vec::new(),
            whole: Sha256::new(),
            total: 0,
        }
    }

    fn close_current(&mut self) -> io::Result<()> {
        if let Some((mut file, hasher, size)) = self.current.take() {
            file.flush()?;
            file.into_inner().map_err(|e| e.into_error())?.sync_all()?;
            self.done.push(PackedPart {
                path: part_path(&self.prefix, self.done.len()),
                size,
                sha256: hasher.finalize().into(),
            });
        }
        Ok(())
    }

    fn finish(mut self) -> io::Result<(Vec<PackedPart>, u64, [u8; 32])> {
        self.close_current()?;
        Ok((self.done, self.total, self.whole.finalize().into()))
    }
}

impl Write for SplitWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if matches!(&self.current, Some((_, _, size)) if *size >= self.part_size) {
            self.close_current()?;
        }
        if self.current.is_none() {
            let path = part_path(&self.prefix, self.done.len());
            let file = File::options().write(true).create_new(true).open(&path)?;
            self.current = Some((BufWriter::new(file), Sha256::new(), 0));
        }
        let Some((file, hasher, size)) = self.current.as_mut() else {
            return Err(io::Error::other("no open part"));
        };
        let room = usize::try_from(self.part_size - *size).unwrap_or(usize::MAX);
        let chunk = &buf[..buf.len().min(room)];
        file.write_all(chunk)?;
        hasher.update(chunk);
        self.whole.update(chunk);
        *size += chunk.len() as u64;
        self.total += chunk.len() as u64;
        Ok(chunk.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.current.as_mut() {
            Some((file, _, _)) => file.flush(),
            None => Ok(()),
        }
    }
}

fn regular_bytes(dir: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = std::fs::metadata(entry.path())?;
        if meta.is_dir() {
            total += regular_bytes(&entry.path())?;
        } else {
            total += meta.len();
        }
    }
    Ok(total)
}

/// Pack an engine onedir as `<prefix>.tar.zst.NNN` parts of at most `part_size` bytes.
/// Links are stored as the files they point to, so the archive holds only directories
/// and regular files under one `txtcaster-engine/` root.
pub fn pack_engine(
    dir: &Path,
    prefix: &Path,
    part_size: u64,
    level: i32,
    threads: u32,
) -> io::Result<Packed> {
    if part_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "part size is zero",
        ));
    }
    if !dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", dir.display()),
        ));
    }
    let unpacked_size = regular_bytes(dir)?;
    let mut encoder =
        zstd::stream::write::Encoder::new(SplitWriter::new(prefix, part_size), level)?;
    if threads > 1 {
        encoder.multithread(threads)?;
    }
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(true);
    builder.sparse(false);
    builder.mode(tar::HeaderMode::Deterministic);
    builder.append_dir_all(ARCHIVE_ROOT, dir)?;
    let encoder = builder.into_inner()?;
    let splitter = encoder.finish()?;
    let (parts, size, sha256) = splitter.finish()?;
    Ok(Packed {
        parts,
        size,
        sha256,
        unpacked_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn noise(len: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect()
    }

    fn tree(root: &Path) {
        std::fs::create_dir_all(root.join("_internal/lib")).expect("dirs");
        std::fs::write(root.join("txtcaster-engine"), b"#!engine\n").expect("bin");
        std::fs::write(root.join("_internal/lib/model.bin"), noise(20_000)).expect("lib");
        std::fs::write(root.join("_internal/notes.txt"), b"hello engine").expect("notes");
    }

    fn concat(parts: &[PackedPart]) -> Vec<u8> {
        let mut all = Vec::new();
        for part in parts {
            all.extend(std::fs::read(&part.path).expect("part"));
        }
        all
    }

    #[test]
    fn a_packed_engine_splits_into_parts_whose_hashes_and_sizes_add_up() {
        let src = tempfile::tempdir().expect("src");
        tree(src.path());
        let out = tempfile::tempdir().expect("out");
        let prefix = out.path().join("txtcaster-engine-0.1.0-linux-x86_64-cpu");
        let packed = pack_engine(src.path(), &prefix, 1024, 1, 1).expect("pack");

        assert!(
            packed.parts.len() > 1,
            "a 1 KiB part size must split: {packed:?}"
        );
        for (index, part) in packed.parts.iter().enumerate() {
            assert_eq!(part.path, part_path(&prefix, index));
            let bytes = std::fs::read(&part.path).expect("part");
            assert!(bytes.len() as u64 <= 1024);
            assert_eq!(bytes.len() as u64, part.size);
            assert_eq!(<[u8; 32]>::from(Sha256::digest(&bytes)), part.sha256);
        }
        let whole = concat(&packed.parts);
        assert_eq!(whole.len() as u64, packed.size);
        assert_eq!(<[u8; 32]>::from(Sha256::digest(&whole)), packed.sha256);
        assert_eq!(packed.unpacked_size, 9 + 20_000 + 12);
    }

    #[test]
    fn the_parts_decode_back_to_the_same_tree_under_one_root() {
        let src = tempfile::tempdir().expect("src");
        tree(src.path());
        let out = tempfile::tempdir().expect("out");
        let packed = pack_engine(src.path(), &out.path().join("e"), 777, 3, 1).expect("pack");
        let mut decoded = Vec::new();
        zstd::stream::read::Decoder::new(concat(&packed.parts).as_slice())
            .expect("decoder")
            .read_to_end(&mut decoded)
            .expect("decode");
        let mut archive = tar::Archive::new(decoded.as_slice());
        let mut seen = Vec::new();
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let path = entry.path().expect("path").into_owned();
            assert!(path.starts_with(ARCHIVE_ROOT), "{path:?}");
            if entry.header().entry_type().is_file() {
                let mut body = Vec::new();
                entry.read_to_end(&mut body).expect("body");
                let rel = path.strip_prefix(ARCHIVE_ROOT).expect("rel");
                assert_eq!(body, std::fs::read(src.path().join(rel)).expect("source"));
                seen.push(rel.to_path_buf());
            }
        }
        seen.sort();
        assert_eq!(
            seen,
            vec![
                PathBuf::from("_internal/lib/model.bin"),
                PathBuf::from("_internal/notes.txt"),
                PathBuf::from("txtcaster-engine"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_onedir_is_packed_as_the_file_it_points_to() {
        let src = tempfile::tempdir().expect("src");
        tree(src.path());
        std::os::unix::fs::symlink("notes.txt", src.path().join("_internal/alias.txt"))
            .expect("link");
        let out = tempfile::tempdir().expect("out");
        let packed = pack_engine(src.path(), &out.path().join("e"), 1 << 20, 1, 1).expect("pack");
        let mut decoded = Vec::new();
        zstd::stream::read::Decoder::new(concat(&packed.parts).as_slice())
            .expect("decoder")
            .read_to_end(&mut decoded)
            .expect("decode");
        let mut archive = tar::Archive::new(decoded.as_slice());
        for entry in archive.entries().expect("entries") {
            let entry = entry.expect("entry");
            let kind = entry.header().entry_type();
            assert!(
                kind.is_file() || kind.is_dir(),
                "only files and dirs: {kind:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_sparse_file_is_packed_as_a_plain_regular_entry() {
        use std::io::{Seek, SeekFrom, Write as _};
        let src = tempfile::tempdir().expect("src");
        tree(src.path());
        let mut sparse = File::create(src.path().join("_internal/sparse.bin")).expect("sparse");
        sparse.seek(SeekFrom::Start(1 << 20)).expect("seek");
        sparse.write_all(b"end").expect("tail");
        drop(sparse);
        let out = tempfile::tempdir().expect("out");
        let packed = pack_engine(src.path(), &out.path().join("e"), 1 << 20, 1, 1).expect("pack");
        let mut decoded = Vec::new();
        zstd::stream::read::Decoder::new(concat(&packed.parts).as_slice())
            .expect("decoder")
            .read_to_end(&mut decoded)
            .expect("decode");
        let mut archive = tar::Archive::new(decoded.as_slice());
        let mut found = false;
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            if entry.path().expect("path").ends_with("sparse.bin") {
                assert_eq!(entry.header().entry_type(), tar::EntryType::Regular);
                let mut body = Vec::new();
                entry.read_to_end(&mut body).expect("body");
                assert_eq!(body.len(), (1 << 20) + 3);
                assert_eq!(&body[body.len() - 3..], b"end");
                found = true;
            }
        }
        assert!(found);
    }

    #[test]
    fn packing_never_overwrites_existing_parts() {
        let src = tempfile::tempdir().expect("src");
        tree(src.path());
        let out = tempfile::tempdir().expect("out");
        let prefix = out.path().join("e");
        std::fs::write(part_path(&prefix, 0), b"keep me").expect("existing");
        assert!(pack_engine(src.path(), &prefix, 1 << 20, 1, 1).is_err());
        assert_eq!(
            std::fs::read(part_path(&prefix, 0)).expect("kept"),
            b"keep me"
        );
    }
}
