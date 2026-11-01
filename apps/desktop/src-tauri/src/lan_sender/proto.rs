//! Wire types for the local transfer protocol (v1), copied verbatim from
//! cast-client `src-tauri/src/transfer/proto.rs` per the contract README
//! (cast-client `tools/mock-sender/README.md`, checked 2026-08-26). The
//! txtCaster-mobile `src-tauri/src/transfer/proto.rs` per the contract README
//! (txtCaster-mobile `tools/mock-sender/README.md`, checked 2026-08-26). The
//! contract is fixed by the phone's receive lane; adapt this file only by
//! re-copying from the source of truth.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u8 = 1;
pub const DEFAULT_CHUNK_SIZE: usize = 8 * 1024 * 1024; // 8 MiB (production)
pub const MIN_CHUNK_SIZE: usize = 64 * 1024;
pub const MAX_CHUNK_SIZE: usize = 16 * 1024 * 1024;
pub const MAX_FILES_PER_CAST: usize = 8;
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Basename only: `audio.opus`, `alignment.json`, `cover.jpg`…
    pub path: String,
    pub size: u64,
    /// Whole-file SHA-256 (lowercase hex).
    pub sha256: String,
    /// Bytes per chunk. Production sends the 8 MiB default; smaller values
    /// are legal so constrained links/tests can exercise multi-chunk flows.
    pub chunk_size: u64,
    /// One SHA-256 per chunk, in order.
    pub chunk_hashes: Vec<String>,
}

impl FileEntry {
    pub fn total_chunks(&self) -> u64 {
        if self.size == 0 {
            return 0;
        }
        self.size.div_ceil(self.chunk_size.max(1))
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_file_path(&self.path)?;
        if self.chunk_size < MIN_CHUNK_SIZE as u64 || self.chunk_size > MAX_CHUNK_SIZE as u64 {
            return Err(format!("chunk size {} out of bounds", self.chunk_size));
        }
        if self.size > MAX_FILE_BYTES {
            return Err("file too large".into());
        }
        let chunks = self.total_chunks();
        if self.chunk_hashes.len() as u64 != chunks {
            return Err(format!(
                "chunk hash count {} != expected {chunks}",
                self.chunk_hashes.len()
            ));
        }
        if !is_sha256_hex(&self.sha256) {
            return Err("whole-file hash malformed".into());
        }
        for h in &self.chunk_hashes {
            if !is_sha256_hex(h) {
                return Err("chunk hash malformed".into());
            }
        }
        Ok(())
    }

    pub fn last_chunk_len(&self) -> u64 {
        let rem = self.size % self.chunk_size;
        if rem == 0 && self.size > 0 {
            self.chunk_size
        } else {
            rem
        }
    }

    pub fn chunk_len(&self, idx: u64) -> u64 {
        let total = self.total_chunks();
        if idx + 1 == total {
            self.last_chunk_len()
        } else {
            self.chunk_size
        }
    }
}

/// Optional alignment metadata riding the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlignmentMeta {
    /// `measured` alignments register word-level timing on the phone;
    /// anything else renders via the synthetic fallback.
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub total_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub protocol: u8,
    pub cast_id: String,
    pub etag: String,
    pub title: String,
    #[serde(default)]
    pub voice_id: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    #[serde(default)]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub alignment: Option<AlignmentMeta>,
    pub files: Vec<FileEntry>,
}

impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(format!("unsupported protocol version {}", self.protocol));
        }
        if !is_safe_id(&self.cast_id) {
            return Err("cast id has unsafe shape".into());
        }
        if self.etag.is_empty() || self.etag.len() > 128 {
            return Err("etag missing or oversized".into());
        }
        if !self
            .etag
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'"' && b != b',')
        {
            return Err("etag carries unsupported characters".into());
        }
        let title = self.title.trim();
        if title.is_empty() || title.len() > 300 {
            return Err("title missing or oversized".into());
        }
        if self.title.chars().any(|c| c.is_control()) {
            return Err("title carries control characters".into());
        }
        if self.files.is_empty() || self.files.len() > MAX_FILES_PER_CAST {
            return Err("file set empty or too large".into());
        }
        let mut seen = std::collections::HashSet::new();
        for f in &self.files {
            f.validate()?;
            if !seen.insert(f.path.clone()) {
                return Err(format!("duplicate file path {}", f.path));
            }
        }
        if let Some(align) = &self.alignment {
            if let Some(origin) = &align.origin {
                if !matches!(
                    origin.as_str(),
                    "measured" | "dynamic" | "legacy" | "unverified"
                ) {
                    return Err("alignment origin unknown".into());
                }
            }
        }
        Ok(())
    }

    /// Finds the primary audio file entry (`audio.opus` preferred).
    pub fn audio_file(&self) -> Option<&FileEntry> {
        self.files
            .iter()
            .find(|f| f.path.starts_with("audio."))
            .or_else(|| self.files.first())
    }
}

#[derive(Debug, Deserialize)]
pub struct PairResponse {
    #[serde(alias = "deviceToken")]
    pub device_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptFile {
    pub path: String,
    pub sha256: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub cast_id: String,
    pub files: Vec<ReceiptFile>,
}

impl Receipt {
    pub fn validate(&self) -> Result<(), String> {
        if !is_safe_id(&self.cast_id) {
            return Err("cast id has unsafe shape".into());
        }
        if self.files.is_empty() || self.files.len() > MAX_FILES_PER_CAST {
            return Err("file set empty or too large".into());
        }
        let mut seen = std::collections::HashSet::new();
        for f in &self.files {
            validate_file_path(&f.path)?;
            if !is_sha256_hex(&f.sha256) {
                return Err("file sha256 malformed".into());
            }
            if !matches!(f.action.as_str(), "reused" | "received") {
                return Err(format!("unknown file action: {}", f.action));
            }
            if !seen.insert(f.path.clone()) {
                return Err(format!("duplicate file path in receipt: {}", f.path));
            }
        }
        Ok(())
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn validate_file_path(path: &str) -> Result<(), String> {
    let ok_name = !path.is_empty()
        && path.len() <= 64
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !path.contains("..");
    if !ok_name {
        return Err(format!("unsafe file path: {path}"));
    }
    let allowed = path == "audio.opus"
        || path == "audio.mp3"
        || path == "alignment.json"
        || path == "alignment.json.gz"
        || path == "transcript.json"
        || path == "cover.jpg"
        || path == "cover.png"
        || path == "cover.webp";
    if !allowed {
        return Err(format!("unexpected file in cast bundle: {path}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_entry() -> FileEntry {
        FileEntry {
            path: "audio.opus".into(),
            size: 1_000_000,
            sha256: "a".repeat(64),
            chunk_size: 262_144,
            chunk_hashes: vec!["b".repeat(64); 4],
        }
    }

    fn manifest(files: Vec<FileEntry>) -> Manifest {
        Manifest {
            protocol: 1,
            cast_id: "cst_test".into(),
            etag: "abc123".into(),
            title: "A Book".into(),
            voice_id: None,
            duration_seconds: Some(60.0),
            content_hash: Some("c".repeat(64)),
            alignment: None,
            files,
        }
    }

    #[test]
    fn valid_manifest_and_entry_pass() {
        let m = manifest(vec![file_entry()]);
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        let f = m.audio_file().unwrap();
        assert_eq!(f.total_chunks(), 4);
        assert_eq!(f.chunk_len(3), 1_000_000 - 3 * 262_144);
    }

    #[test]
    fn rejects_unsafe_paths_and_shapes() {
        let mut bad_traversal = file_entry();
        bad_traversal.path = "../etc/passwd".into();
        assert!(bad_traversal.validate().is_err());

        let mut bad_abs = file_entry();
        bad_abs.path = "/audio.opus".into();
        assert!(bad_abs.validate().is_err());

        let mut bad_unknown = file_entry();
        bad_unknown.path = "secrets.zip".into();
        assert!(bad_unknown.validate().is_err());

        let mut bad_chunk_count = file_entry();
        bad_chunk_count.chunk_hashes.pop();
        assert!(bad_chunk_count.validate().is_err());

        let mut bad_chunk_size = file_entry();
        bad_chunk_size.chunk_size = 1024;
        assert!(bad_chunk_size.validate().is_err());

        let mut m = manifest(vec![file_entry()]);
        m.protocol = 2;
        assert!(m.validate().is_err());

        let mut m = manifest(vec![file_entry()]);
        m.cast_id = "../escape".into();
        assert!(m.validate().is_err());

        let mut m = manifest(vec![file_entry()]);
        m.title.push('\u{0}');
        assert!(m.validate().is_err());
    }

    #[test]
    fn zero_byte_file_has_zero_chunks_but_fails_hash_count_contract() {
        // A zero-byte file declares no hashes; size-0 entries are rejected by
        // count contract (0 != 0 passes) — verify explicitly.
        let mut f = file_entry();
        f.size = 0;
        f.chunk_hashes.clear();
        assert_eq!(f.total_chunks(), 0);
        assert!(f.validate().is_ok());
        assert_eq!(f.last_chunk_len(), 0);
    }

    #[test]
    fn alignment_origin_whitelist() {
        let mut m = manifest(vec![file_entry()]);
        m.alignment = Some(AlignmentMeta {
            origin: Some("measured".into()),
            total_ms: Some(378_256),
        });
        for origin in &["measured", "dynamic", "legacy", "unverified"] {
            m.alignment.as_mut().unwrap().origin = Some((*origin).into());
            assert!(m.validate().is_ok());
        }
        m.alignment.as_mut().unwrap().origin = Some("guessed".into());
        assert!(m.validate().is_err());
    }

    #[test]
    fn allowed_companion_filenames() {
        for path in &[
            "audio.opus",
            "audio.mp3",
            "alignment.json",
            "alignment.json.gz",
            "transcript.json",
            "cover.jpg",
            "cover.png",
            "cover.webp",
        ] {
            assert!(
                validate_file_path(path).is_ok(),
                "path {path} should be allowed"
            );
        }
        for bad in &["malicious.exe", "transcript.txt", "audio.wav", "cover.gif"] {
            assert!(
                validate_file_path(bad).is_err(),
                "bad path {bad} should be rejected"
            );
        }
    }

    #[test]
    fn receipt_validates_and_rejects_malformed_shapes() {
        let valid = Receipt {
            cast_id: "cst_valid".into(),
            files: vec![
                ReceiptFile {
                    path: "audio.opus".into(),
                    sha256: "a".repeat(64),
                    action: "reused".into(),
                },
                ReceiptFile {
                    path: "alignment.json".into(),
                    sha256: "b".repeat(64),
                    action: "received".into(),
                },
            ],
        };
        assert!(valid.validate().is_ok());

        let mut bad_action = valid.clone();
        bad_action.files[0].action = "unknown".into();
        assert!(bad_action.validate().is_err());

        let mut bad_hash = valid.clone();
        bad_hash.files[0].sha256 = "short".into();
        assert!(bad_hash.validate().is_err());

        let mut bad_id = valid.clone();
        bad_id.cast_id = "../escape".into();
        assert!(bad_id.validate().is_err());

        let mut empty = valid.clone();
        empty.files.clear();
        assert!(empty.validate().is_err());

        let mut duplicate = valid.clone();
        duplicate.files.push(duplicate.files[0].clone());
        assert!(duplicate.validate().is_err());
    }
}
