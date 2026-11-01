use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use rand::RngCore;
use sha2::{Digest, Sha256};

use super::proto;

pub const DEFAULT_PORT: u16 = 47317;
pub const PAIR_FAILURE_LIMIT: u32 = 10;
/// A minted pairing secret is accepted only while `elapsed < PAIR_SECRET_TTL`
/// on the desktop's monotonic clock (the phone never judges time). Prior art:
/// RFC 6749 §4.1.2 recommends at most ten minutes for a single-use code.
pub const PAIR_SECRET_TTL: Duration = Duration::from_secs(300);
/// Injectable monotonic clock so tests can cross the TTL boundary exactly.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;
const EMIT_INTERVAL_MS: u128 = 200;
const DEVICE_NAME_LIMIT: usize = 80;
const MAX_PAIRED_DEVICES: usize = 8;
/// TLS handshake deadline. The phone abandons its own connect at 2 s
/// (`CONNECT_TIMEOUT` in the mobile client), so this only ever fires for a peer
/// that stopped talking mid-handshake — while still bounding the task, socket
/// and buffers that peer was holding.
pub const DEFAULT_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(12);
/// Deadline for a complete request head. hyper's default (30 s) is inert unless
/// a `Timer` is installed, so the listener installs one (server.rs) and this
/// value is what actually applies.
pub const DEFAULT_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct CastSource {
    pub cast_id: String,
    pub title: String,
    pub path: PathBuf,
    pub cover_path: Option<PathBuf>,
    pub alignment_path: Option<PathBuf>,
    pub duration_seconds: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct FileMeta {
    pub size: u64,
    pub modified_ms: u128,
    pub sha256: String,
    pub chunk_hashes: Vec<String>,
    pub etag: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CastProgress {
    pub cast_id: String,
    pub title: String,
    pub bytes_sent: u64,
    pub bytes_total: u64,
    pub sent_complete: bool,
    pub missing: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub phase: String,
    pub paired_devices: u32,
    pub device_names: Vec<String>,
    pub pair_locked: bool,
    pub accepted_connections: u64,
    /// Connections refused because the listener was already at capacity.
    /// Surfaced as a number rather than one log line per shed: the default
    /// filter is `info` with no file layer, so a per-shed line would be either
    /// invisible (`debug`) or unbounded stdout churn under a deliberate flood.
    pub shed_connections: u64,
    pub casts: Vec<CastProgress>,
}

pub enum PairOutcome {
    Token(String),
    Rejected,
}

pub struct CastEntry {
    pub source: CastSource,
    pub wire_path: &'static str,
    pub size_hint: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub sent_complete: AtomicBool,
    pub missing: AtomicBool,
    pub completed_files: Mutex<HashSet<String>>,
    meta: Mutex<Option<Arc<FileMeta>>>,
    cover_meta: Mutex<Option<Arc<CoverMeta>>>,
    alignment_meta: Mutex<Option<Arc<AlignmentFileMeta>>>,
}

impl CastEntry {
    pub fn expected_file_names(&self) -> Vec<String> {
        let mut names = vec![self.wire_path.to_string()];
        if let Some(cover) = self
            .cover_meta
            .lock()
            .expect("cover_meta poisoned")
            .as_ref()
        {
            names.push(cover.wire.to_string());
        } else if let Some((_, wire)) = cover_source_for_source(&self.source) {
            names.push(wire.to_string());
        }
        if let Some(align) = self
            .alignment_meta
            .lock()
            .expect("alignment_meta poisoned")
            .as_ref()
        {
            names.push(align.wire.to_string());
        } else if let Some((_, wire)) = alignment_source_for_source(&self.source) {
            names.push(wire.to_string());
        }
        names
    }

    pub fn is_all_files_complete(&self) -> bool {
        let expected = self.expected_file_names();
        let completed = self
            .completed_files
            .lock()
            .expect("completed_files poisoned");
        expected.iter().all(|name| completed.contains(name))
    }
}

pub type EventSink = Arc<dyn Fn(SessionSnapshot) + Send + Sync>;

pub struct SessionState {
    pub casts: Vec<Arc<CastEntry>>,
    pub chunk_size: u64,
    secret: Mutex<SecretSlot>,
    clock: Mutex<Clock>,
    tokens: Mutex<HashSet<String>>,
    device_names: Mutex<Vec<String>>,
    pub accepted_connections: AtomicU64,
    pub shed_connections: AtomicU64,
    /// Pre-auth deadlines. Production values are the `DEFAULT_*` constants;
    /// tests shrink them so a stall is observable in milliseconds.
    pub handshake_deadline: Duration,
    pub header_read_timeout: Duration,
    events: EventSink,
    last_emit: Mutex<Instant>,
}

struct SecretSlot {
    value: Option<String>,
    minted_at: Option<Instant>,
    consumed: HashSet<[u8; 32]>,
    failures: u32,
}

pub fn wire_file_name(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp3") => Some("audio.mp3"),
        Some("opus") => Some("audio.opus"),
        _ => None,
    }
}

/// Cover-art candidates, in preference order. The wire contract already
/// admits all three (proto::validate_file_path); the first sibling present
/// beside the audio file rides the manifest.
const COVER_CANDIDATES: [&str; 3] = ["cover.png", "cover.jpg", "cover.webp"];

/// Disk path + wire name of the cover sibling, if one is present beside the
/// audio file. Absence is the normal case and never an error.
pub fn cover_source_for(audio_path: &Path) -> Option<(PathBuf, &'static str)> {
    let dir = audio_path.parent()?;
    COVER_CANDIDATES.into_iter().find_map(|name| {
        let path = dir.join(name);
        path.is_file().then_some((path, name))
    })
}

/// Disk path + wire name of the cover, prioritizing explicit source.cover_path
/// then falling back to sibling lookup beside the audio file.
pub fn cover_source_for_source(source: &CastSource) -> Option<(PathBuf, &'static str)> {
    if let Some(ref cover) = source.cover_path {
        if cover.is_file() {
            let wire = match cover
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .as_deref()
            {
                Some("png") => "cover.png",
                Some("jpg") | Some("jpeg") => "cover.jpg",
                Some("webp") => "cover.webp",
                _ => "cover.png",
            };
            return Some((cover.clone(), wire));
        }
    }
    cover_source_for(&source.path)
}

/// Disk path + wire name of alignment.json sibling, if present beside the audio file.
pub fn alignment_source_for(audio_path: &Path) -> Option<(PathBuf, &'static str)> {
    let dir = audio_path.parent()?;
    let path = dir.join("alignment.json");
    path.is_file().then_some((path, "alignment.json"))
}

/// Disk path + wire name of alignment.json/transcript.json, prioritizing explicit
/// source.alignment_path then falling back to sibling lookup beside the audio file.
pub fn alignment_source_for_source(source: &CastSource) -> Option<(PathBuf, &'static str)> {
    if let Some(ref align) = source.alignment_path {
        if align.is_file() {
            let wire = match align
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_ascii_lowercase())
                .as_deref()
            {
                Some("alignment.json.gz") => "alignment.json.gz",
                Some("transcript.json") => "transcript.json",
                _ => "alignment.json",
            };
            return Some((align.clone(), wire));
        }
    }
    alignment_source_for(&source.path)
}

/// A hashed cover file ready to ride the manifest and the range server.
#[derive(Debug, Clone)]
pub struct CoverMeta {
    pub path: PathBuf,
    pub wire: &'static str,
    pub meta: Arc<FileMeta>,
}

/// A hashed alignment file ready to ride the manifest and the range server.
#[derive(Debug, Clone)]
pub struct AlignmentFileMeta {
    pub path: PathBuf,
    pub wire: &'static str,
    pub meta: Arc<FileMeta>,
    pub proto_meta: proto::AlignmentMeta,
}

impl SessionState {
    pub fn new(
        sources: Vec<CastSource>,
        chunk_size: u64,
        events: EventSink,
    ) -> Result<Self, String> {
        if sources.is_empty() {
            return Err("nothing selected to send".into());
        }
        let mut seen = HashSet::new();
        let mut casts = Vec::with_capacity(sources.len());
        for mut source in sources {
            if !is_safe_cast_id(&source.cast_id) {
                return Err(format!("cast id has unsafe shape: {}", source.cast_id));
            }
            if !seen.insert(source.cast_id.clone()) {
                return Err(format!("duplicate cast id {}", source.cast_id));
            }
            source.title = sanitize_title(&source.title);
            let wire_path = wire_file_name(&source.path)
                .ok_or_else(|| format!("'{}' is not a sendable audio file", source.title))?;
            let size_hint = std::fs::metadata(&source.path)
                .map(|m| m.len())
                .unwrap_or(0);
            let missing = size_hint == 0 && !source.path.is_file();
            casts.push(Arc::new(CastEntry {
                source,
                wire_path,
                size_hint: AtomicU64::new(size_hint),
                bytes_sent: AtomicU64::new(0),
                sent_complete: AtomicBool::new(false),
                missing: AtomicBool::new(missing),
                completed_files: Mutex::new(HashSet::new()),
                meta: Mutex::new(None),
                cover_meta: Mutex::new(None),
                alignment_meta: Mutex::new(None),
            }));
        }
        Ok(SessionState {
            casts,
            chunk_size,
            secret: Mutex::new(SecretSlot {
                value: None,
                minted_at: None,
                consumed: HashSet::new(),
                failures: 0,
            }),
            clock: Mutex::new(Arc::new(Instant::now)),
            tokens: Mutex::new(HashSet::new()),
            device_names: Mutex::new(Vec::new()),
            accepted_connections: AtomicU64::new(0),
            shed_connections: AtomicU64::new(0),
            handshake_deadline: DEFAULT_HANDSHAKE_DEADLINE,
            header_read_timeout: DEFAULT_HEADER_READ_TIMEOUT,
            events,
            last_emit: Mutex::new(Instant::now()),
        })
    }

    /// Replaces the wall clock for tests. Production keeps `Instant::now`.
    pub fn set_clock(&self, clock: Clock) {
        *self.clock.lock().expect("clock poisoned") = clock;
    }

    fn now(&self) -> Instant {
        (self.clock.lock().expect("clock poisoned"))()
    }

    pub fn mint_secret(&self) -> String {
        let secret = random_hex_128();
        let previously_consumed = {
            let mut slot = self.secret.lock().expect("secret slot poisoned");
            let now = self.now();
            slot.value = Some(secret.clone());
            slot.minted_at = Some(now);
            slot.failures = 0;
            slot.consumed.len()
        };
        tracing::info!(
            secret_len = secret.len(),
            previously_consumed,
            "lan sender minted a fresh single-use pairing secret"
        );
        self.emit(true);
        secret
    }

    pub fn try_pair(&self, supplied: &str, device_name: &str) -> PairOutcome {
        let matched = {
            let mut slot = self.secret.lock().expect("secret slot poisoned");
            // The clock is read AFTER the lock is held: a request that queued
            // at 299.99 s and acquires the slot past 300 s is judged at the
            // moment of judgement, never with a reading taken while waiting.
            let now = self.now();
            // Expiry is judged here, under the same lock as the match, on the
            // desktop's monotonic clock: a secret older than PAIR_SECRET_TTL is
            // burned into `consumed` so every later presentation takes the
            // replay branch (uniform 403, never counted toward the lockout).
            if let (Some(value), Some(minted_at)) = (slot.value.as_deref(), slot.minted_at) {
                if now.saturating_duration_since(minted_at) >= PAIR_SECRET_TTL {
                    let stale = digest_of(value);
                    slot.consumed.insert(stale);
                    slot.value = None;
                    slot.minted_at = None;
                    tracing::warn!(
                        ttl_secs = PAIR_SECRET_TTL.as_secs(),
                        "lan sender pairing secret expired, burned until a fresh mint"
                    );
                }
            }
            let supplied_digest = digest_of(supplied);
            let minted = slot.value.is_some();
            let matched = slot
                .value
                .as_deref()
                .is_some_and(|expected| digest_of(expected) == supplied_digest);
            let replay = slot.consumed.contains(&supplied_digest);
            if matched {
                slot.consumed.insert(supplied_digest);
                slot.value = None;
                tracing::info!(
                    supplied_len = supplied.len(),
                    consumed_total = slot.consumed.len(),
                    "lan sender pair accepted, single-use secret consumed"
                );
            } else if !replay {
                slot.failures += 1;
                let cause = if supplied.is_empty() {
                    "missing_or_empty_secret_header"
                } else if minted {
                    "secret_mismatch"
                } else {
                    "no_secret_minted_or_already_burned"
                };
                tracing::warn!(
                    cause,
                    supplied_len = supplied.len(),
                    failures = slot.failures,
                    "lan sender pair rejected, peer sees the byte-identical 403"
                );
                if slot.failures >= PAIR_FAILURE_LIMIT {
                    slot.value = None;
                    tracing::warn!(
                        failures = slot.failures,
                        "lan sender pair lockout, secret burned until a fresh mint"
                    );
                }
            } else {
                tracing::warn!(
                    cause = "replay_of_consumed_secret",
                    supplied_len = supplied.len(),
                    failures = slot.failures,
                    "lan sender pair rejected, peer sees the byte-identical 403"
                );
            }
            matched
        };
        if !matched {
            self.emit(true);
            return PairOutcome::Rejected;
        }
        let token = format!("tok_{}", random_hex_128());
        self.tokens
            .lock()
            .expect("token set poisoned")
            .insert(token.clone());
        {
            let mut names = self.device_names.lock().expect("device names poisoned");
            if names.len() < MAX_PAIRED_DEVICES {
                names.push(sanitize_device_name(device_name));
            } else {
                tracing::warn!(
                    devices = names.len(),
                    "lan sender device roster full, token issued but device not listed"
                );
            }
        }
        self.emit(true);
        PairOutcome::Token(token)
    }

    pub fn token_valid(&self, token: &str) -> bool {
        self.tokens
            .lock()
            .expect("token set poisoned")
            .contains(token)
    }

    /// Test probe: true while some caller holds the secret slot lock.
    #[cfg(test)]
    pub fn secret_slot_held(&self) -> bool {
        self.secret.try_lock().is_err()
    }

    pub fn pair_locked(&self) -> bool {
        let slot = self.secret.lock().expect("secret slot poisoned");
        slot.failures >= PAIR_FAILURE_LIMIT
    }

    pub fn cast(&self, cast_id: &str) -> Option<Arc<CastEntry>> {
        self.casts
            .iter()
            .find(|c| c.source.cast_id == cast_id)
            .cloned()
    }

    pub fn record_sent(&self, entry: &CastEntry, bytes: u64) {
        entry.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
        self.emit(false);
    }

    pub fn record_range_complete(
        &self,
        entry: &CastEntry,
        file_name: &str,
        end_inclusive: u64,
        size: u64,
    ) {
        if (size > 0 && end_inclusive == size - 1) || size == 0 {
            let mut completed = entry
                .completed_files
                .lock()
                .expect("completed_files poisoned");
            completed.insert(file_name.to_string());
            drop(completed);
            if entry.is_all_files_complete() {
                entry.sent_complete.store(true, Ordering::Relaxed);
            }
        }
        self.emit(true);
    }

    pub async fn record_receipt(&self, receipt: &proto::Receipt) -> Result<bool, String> {
        receipt.validate()?;
        let Some(entry) = self.cast(&receipt.cast_id) else {
            return Err("unknown cast id".into());
        };
        let meta = self
            .ensure_meta(&entry)
            .await
            .map_err(|e| format!("manifest source file missing: {e:?}"))?;
        let cover = self.ensure_cover_meta(&entry).await.unwrap_or(None);
        let alignment = self.ensure_alignment_meta(&entry).await.unwrap_or(None);
        let manifest = self.manifest_for(&entry, &meta, cover.as_deref(), alignment.as_deref());

        for rf in &receipt.files {
            let matches_manifest = manifest
                .files
                .iter()
                .any(|mf| mf.path == rf.path && mf.sha256 == rf.sha256);
            if !matches_manifest {
                return Err(format!(
                    "receipt file {} sha256 mismatch or unknown",
                    rf.path
                ));
            }
        }
        let all_accounted = manifest.files.iter().all(|mf| {
            receipt
                .files
                .iter()
                .any(|rf| rf.path == mf.path && rf.sha256 == mf.sha256)
        });
        if !all_accounted {
            return Err("receipt missing manifest files".into());
        }
        {
            let mut completed = entry
                .completed_files
                .lock()
                .expect("completed_files poisoned");
            for rf in &receipt.files {
                completed.insert(rf.path.clone());
            }
        }
        entry.sent_complete.store(true, Ordering::Relaxed);
        self.emit(true);
        Ok(true)
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        let device_names = self
            .device_names
            .lock()
            .expect("device names poisoned")
            .clone();
        let casts: Vec<CastProgress> = self
            .casts
            .iter()
            .map(|entry| {
                let total = entry.size_hint.load(Ordering::Relaxed);
                CastProgress {
                    cast_id: entry.source.cast_id.clone(),
                    title: entry.source.title.clone(),
                    bytes_sent: entry.bytes_sent.load(Ordering::Relaxed).min(total),
                    bytes_total: total,
                    sent_complete: entry.sent_complete.load(Ordering::Relaxed),
                    missing: entry.missing.load(Ordering::Relaxed),
                }
            })
            .collect();
        let paired = device_names.len() as u32;
        let all_sent = casts.iter().all(|c| c.sent_complete || c.missing);
        let any_progress = casts.iter().any(|c| c.bytes_sent > 0);
        let phase = if paired == 0 {
            "waiting"
        } else if all_sent {
            "done"
        } else if any_progress {
            "sending"
        } else {
            "paired"
        };
        SessionSnapshot {
            phase: phase.into(),
            paired_devices: paired,
            device_names,
            pair_locked: self.pair_locked(),
            accepted_connections: self.accepted_connections.load(Ordering::Relaxed),
            shed_connections: self.shed_connections.load(Ordering::Relaxed),
            casts,
        }
    }

    pub fn emit(&self, force: bool) {
        {
            let mut last = self.last_emit.lock().expect("emit clock poisoned");
            if !force && last.elapsed().as_millis() < EMIT_INTERVAL_MS {
                return;
            }
            *last = Instant::now();
        }
        (self.events)(self.snapshot());
    }

    pub async fn ensure_meta(&self, entry: &Arc<CastEntry>) -> Result<Arc<FileMeta>, MetaError> {
        let (size, modified_ms) = match std::fs::metadata(&entry.source.path) {
            Ok(md) => (md.len(), modified_millis(&md)),
            Err(_) => {
                entry.missing.store(true, Ordering::Relaxed);
                self.emit(true);
                return Err(MetaError::Missing);
            }
        };
        entry.missing.store(false, Ordering::Relaxed);
        entry.size_hint.store(size, Ordering::Relaxed);
        {
            let cached = entry.meta.lock().expect("meta slot poisoned");
            if let Some(meta) = cached.as_ref() {
                if meta.size == size && meta.modified_ms == modified_ms {
                    return Ok(meta.clone());
                }
            }
        }
        let key = (entry.source.path.clone(), size, modified_ms);
        if let Some(meta) = hash_cache().lock().expect("hash cache poisoned").get(&key) {
            let meta = meta.clone();
            *entry.meta.lock().expect("meta slot poisoned") = Some(meta.clone());
            return Ok(meta);
        }
        let path = entry.source.path.clone();
        let chunk_size = self.chunk_size;
        let meta = tokio::task::spawn_blocking(move || compute_file_meta(&path, chunk_size))
            .await
            .map_err(|e| MetaError::Io(e.to_string()))?
            .map_err(|e| MetaError::Io(e.to_string()))?;
        let meta = Arc::new(meta);
        hash_cache()
            .lock()
            .expect("hash cache poisoned")
            .insert(key, meta.clone());
        *entry.meta.lock().expect("meta slot poisoned") = Some(meta.clone());
        Ok(meta)
    }

    /// Cover art is optional: a `cover.{png,jpg,webp}` sibling of the audio
    /// file hashes into a manifest entry when present. Absence — including
    /// a cover that vanishes between probe and stat — is `Ok(None)`, never
    /// an error; only real hashing failures surface as `Io`.
    pub async fn ensure_cover_meta(
        &self,
        entry: &CastEntry,
    ) -> Result<Option<Arc<CoverMeta>>, MetaError> {
        let Some((path, wire)) = cover_source_for_source(&entry.source) else {
            return Ok(None);
        };
        let (size, modified_ms) = match std::fs::metadata(&path) {
            Ok(md) => (md.len(), modified_millis(&md)),
            Err(_) => return Ok(None),
        };
        {
            let cached = entry.cover_meta.lock().expect("cover meta slot poisoned");
            if let Some(cover) = cached.as_ref() {
                if cover.path == path
                    && cover.meta.size == size
                    && cover.meta.modified_ms == modified_ms
                {
                    return Ok(Some(cover.clone()));
                }
            }
        }
        let key = (path.clone(), size, modified_ms);
        if let Some(meta) = hash_cache().lock().expect("hash cache poisoned").get(&key) {
            let cover = Arc::new(CoverMeta {
                path,
                wire,
                meta: meta.clone(),
            });
            *entry.cover_meta.lock().expect("cover meta slot poisoned") = Some(cover.clone());
            return Ok(Some(cover));
        }
        let chunk_size = self.chunk_size;
        let hash_path = path.clone();
        let meta = tokio::task::spawn_blocking(move || compute_file_meta(&hash_path, chunk_size))
            .await
            .map_err(|e| MetaError::Io(e.to_string()))?
            .map_err(|e| MetaError::Io(e.to_string()))?;
        let meta = Arc::new(meta);
        hash_cache()
            .lock()
            .expect("hash cache poisoned")
            .insert(key, meta.clone());
        let cover = Arc::new(CoverMeta { path, wire, meta });
        *entry.cover_meta.lock().expect("cover meta slot poisoned") = Some(cover.clone());
        Ok(Some(cover))
    }

    /// Alignment data is optional: an `alignment.json` sibling of the audio
    /// file hashes into a manifest entry and sets alignment metadata when present.
    pub async fn ensure_alignment_meta(
        &self,
        entry: &CastEntry,
    ) -> Result<Option<Arc<AlignmentFileMeta>>, MetaError> {
        let Some((path, wire)) = alignment_source_for_source(&entry.source) else {
            return Ok(None);
        };
        let (size, modified_ms) = match std::fs::metadata(&path) {
            Ok(md) => (md.len(), modified_millis(&md)),
            Err(_) => return Ok(None),
        };
        {
            let cached = entry
                .alignment_meta
                .lock()
                .expect("alignment meta slot poisoned");
            if let Some(alignment) = cached.as_ref() {
                if alignment.path == path
                    && alignment.meta.size == size
                    && alignment.meta.modified_ms == modified_ms
                {
                    return Ok(Some(alignment.clone()));
                }
            }
        }
        let mut total_ms = entry
            .source
            .duration_seconds
            .filter(|&dur| dur > 0.0)
            .map(|dur| (dur * 1000.0) as u64);
        let mut origin = None;
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if total_ms.is_none() {
                    if let Some(ms) = v.get("totalMs").and_then(|m| m.as_u64()) {
                        total_ms = Some(ms);
                    }
                }
                if let Some(orig) = v.get("origin").and_then(|o| o.as_str()) {
                    origin = Some(orig.to_string());
                }
            }
        }
        let proto_meta = proto::AlignmentMeta {
            origin: Some(origin.unwrap_or_else(|| "unverified".to_string())),
            total_ms,
        };

        let key = (path.clone(), size, modified_ms);
        if let Some(meta) = hash_cache().lock().expect("hash cache poisoned").get(&key) {
            let alignment = Arc::new(AlignmentFileMeta {
                path,
                wire,
                meta: meta.clone(),
                proto_meta,
            });
            *entry
                .alignment_meta
                .lock()
                .expect("alignment meta slot poisoned") = Some(alignment.clone());
            return Ok(Some(alignment));
        }
        let chunk_size = self.chunk_size;
        let hash_path = path.clone();
        let meta = tokio::task::spawn_blocking(move || compute_file_meta(&hash_path, chunk_size))
            .await
            .map_err(|e| MetaError::Io(e.to_string()))?
            .map_err(|e| MetaError::Io(e.to_string()))?;
        let meta = Arc::new(meta);
        hash_cache()
            .lock()
            .expect("hash cache poisoned")
            .insert(key, meta.clone());
        let alignment = Arc::new(AlignmentFileMeta {
            path,
            wire,
            meta,
            proto_meta,
        });
        *entry
            .alignment_meta
            .lock()
            .expect("alignment meta slot poisoned") = Some(alignment.clone());
        Ok(Some(alignment))
    }

    pub fn manifest_for(
        &self,
        entry: &CastEntry,
        meta: &FileMeta,
        cover: Option<&CoverMeta>,
        alignment: Option<&AlignmentFileMeta>,
    ) -> proto::Manifest {
        let mut files = vec![proto::FileEntry {
            path: entry.wire_path.to_string(),
            size: meta.size,
            sha256: meta.sha256.clone(),
            chunk_size: self.chunk_size,
            chunk_hashes: meta.chunk_hashes.clone(),
        }];
        // Optional cover art rides as a second hashed entry.
        if let Some(cover) = cover {
            files.push(proto::FileEntry {
                path: cover.wire.to_string(),
                size: cover.meta.size,
                sha256: cover.meta.sha256.clone(),
                chunk_size: self.chunk_size,
                chunk_hashes: cover.meta.chunk_hashes.clone(),
            });
        }
        // Optional alignment rides alongside the files and populates alignment metadata.
        if let Some(align) = alignment {
            files.push(proto::FileEntry {
                path: align.wire.to_string(),
                size: align.meta.size,
                sha256: align.meta.sha256.clone(),
                chunk_size: self.chunk_size,
                chunk_hashes: align.meta.chunk_hashes.clone(),
            });
        }
        proto::Manifest {
            protocol: proto::PROTOCOL_VERSION,
            cast_id: entry.source.cast_id.clone(),
            etag: meta.etag.clone(),
            title: entry.source.title.clone(),
            voice_id: None,
            duration_seconds: entry.source.duration_seconds,
            content_hash: Some(meta.sha256.clone()),
            alignment: alignment.map(|a| a.proto_meta.clone()),
            files,
        }
    }
}

#[derive(Debug)]
pub enum MetaError {
    Missing,
    Io(String),
}

fn random_hex_128() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn digest_of(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

fn is_safe_cast_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn sanitize_title(raw: &str) -> String {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let trimmed: String = cleaned.trim().chars().take(200).collect();
    let trimmed = trimmed.trim_end();
    if trimmed.is_empty() {
        "Audiobook".into()
    } else {
        trimmed.to_string()
    }
}

fn sanitize_device_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(DEVICE_NAME_LIMIT)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "Phone".into()
    } else {
        trimmed.to_string()
    }
}

fn modified_millis(md: &std::fs::Metadata) -> u128 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

type HashCacheKey = (PathBuf, u64, u128);

fn hash_cache() -> &'static Mutex<HashMap<HashCacheKey, Arc<FileMeta>>> {
    static CACHE: OnceLock<Mutex<HashMap<HashCacheKey, Arc<FileMeta>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn compute_file_meta(path: &Path, chunk_size: u64) -> std::io::Result<FileMeta> {
    let mut file = std::fs::File::open(path)?;
    let md = file.metadata()?;
    let size = md.len();
    let modified_ms = modified_millis(&md);
    let mut whole = Sha256::new();
    let mut chunk = Sha256::new();
    let mut chunk_hashes = Vec::new();
    let mut in_chunk: u64 = 0;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        whole.update(&buf[..n]);
        let mut off = 0usize;
        while off < n {
            let take = ((chunk_size - in_chunk) as usize).min(n - off);
            chunk.update(&buf[off..off + take]);
            in_chunk += take as u64;
            off += take;
            if in_chunk == chunk_size {
                chunk_hashes.push(hex::encode(std::mem::take(&mut chunk).finalize()));
                in_chunk = 0;
            }
        }
    }
    if in_chunk > 0 {
        chunk_hashes.push(hex::encode(chunk.finalize()));
    }
    let sha256 = hex::encode(whole.finalize());
    let etag = format!("etag-{}", &sha256[..16]);
    Ok(FileMeta {
        size,
        modified_ms,
        sha256,
        chunk_hashes,
        etag,
    })
}

pub struct ActiveSend {
    pub session: Arc<SessionState>,
    pub server: super::server::ServerHandle,
    pub export_ids: HashSet<String>,
    pub hosts: Vec<String>,
}

#[derive(Default)]
pub struct LanSenderHandle {
    active: Mutex<Option<ActiveSend>>,
}

impl LanSenderHandle {
    pub fn active(&self) -> std::sync::MutexGuard<'_, Option<ActiveSend>> {
        self.active.lock().expect("lan sender handle poisoned")
    }
}

#[derive(serde::Serialize)]
struct QrPayload<'a> {
    v: u8,
    f: &'a str,
    h: &'a [String],
    s: &'a str,
}

pub fn qr_payload(hosts: &[String], fingerprint: &str, secret: &str) -> String {
    serde_json::to_string(&QrPayload {
        v: proto::PROTOCOL_VERSION,
        f: fingerprint,
        h: hosts,
        s: secret,
    })
    .expect("qr payload serializes")
}

pub fn qr_svg(payload: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(payload.as_bytes()).map_err(|e| e.to_string())?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .quiet_zone(true)
        .min_dimensions(200, 200)
        .build())
}

pub fn advertisable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

pub fn advertised_hosts(interfaces: &[IpAddr], port: u16) -> Vec<String> {
    let mut v4: Vec<String> = Vec::new();
    let mut v6: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for ip in interfaces {
        if !advertisable(ip) || !seen.insert(*ip) {
            continue;
        }
        match ip {
            IpAddr::V4(addr) => v4.push(format!("{addr}:{port}")),
            IpAddr::V6(addr) => v6.push(format!("[{addr}]:{port}")),
        }
    }
    v4.extend(v6);
    v4
}

pub fn peer_allowed(ip: &IpAddr) -> bool {
    let canonical = match ip {
        IpAddr::V6(v6) => v6.to_canonical(),
        v4 => *v4,
    };
    match canonical {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn noop_sink() -> EventSink {
        Arc::new(|_| {})
    }

    fn temp_audio(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("book.mp3");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(bytes))
            .expect("write payload");
        (dir, path)
    }

    fn session_for(path: PathBuf, chunk_size: u64) -> SessionState {
        SessionState::new(
            vec![CastSource {
                cast_id: "cst_test".into(),
                title: "A Book".into(),
                path,
                cover_path: None,
                alignment_path: None,
                duration_seconds: Some(60.0),
            }],
            chunk_size,
            noop_sink(),
        )
        .expect("session")
    }

    #[test]
    fn chunk_hashes_cover_exact_multiple_and_remainder() {
        let (_dir, path) = temp_audio(&[7u8; 300_000]);
        let meta = compute_file_meta(&path, 100_000).expect("meta");
        assert_eq!(meta.size, 300_000);
        assert_eq!(meta.chunk_hashes.len(), 3);
        let (_dir2, path2) = temp_audio(&[7u8; 250_000]);
        let meta2 = compute_file_meta(&path2, 100_000).expect("meta");
        assert_eq!(meta2.chunk_hashes.len(), 3);
        assert_eq!(meta2.chunk_hashes[0], meta.chunk_hashes[0]);
        assert_ne!(meta2.chunk_hashes[2], meta.chunk_hashes[2]);
    }

    #[test]
    fn empty_file_has_no_chunk_hashes() {
        let (_dir, path) = temp_audio(&[]);
        let meta = compute_file_meta(&path, 100_000).expect("meta");
        assert_eq!(meta.size, 0);
        assert!(meta.chunk_hashes.is_empty());
    }

    #[test]
    fn etag_is_stable_for_identical_bytes_across_paths() {
        let (_dir, a) = temp_audio(b"identical bytes");
        let (_dir2, b) = temp_audio(b"identical bytes");
        let meta_a = compute_file_meta(&a, 65_536).expect("meta");
        let meta_b = compute_file_meta(&b, 65_536).expect("meta");
        assert_eq!(meta_a.etag, meta_b.etag);
        assert!(meta_a.etag.starts_with("etag-"));
        assert_eq!(meta_a.etag.len(), "etag-".len() + 16);
    }

    #[test]
    fn manifest_built_from_meta_passes_contract_validation() {
        let (_dir, path) = temp_audio(&[3u8; 500_000]);
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        let meta = compute_file_meta(&entry.source.path, session.chunk_size).expect("meta");
        let manifest = session.manifest_for(&entry, &meta, None, None);
        assert!(manifest.validate().is_ok(), "{:?}", manifest.validate());
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "audio.mp3");
        assert_eq!(manifest.content_hash.as_deref(), Some(meta.sha256.as_str()));
    }

    fn temp_cover(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        // Content is arbitrary: hashing is content-agnostic, and the wire
        // contract validates shape, not pixels.
        let path = dir.path().join(name);
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(bytes))
            .expect("write cover");
        path
    }

    fn temp_file(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(bytes))
            .expect("write temp file");
        path
    }

    #[tokio::test]
    async fn cover_absent_is_none_and_manifest_lists_audio_only() {
        let (_dir, path) = temp_audio(&[3u8; 100_000]);
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        let cover = session
            .ensure_cover_meta(&entry)
            .await
            .expect("absence is not an error");
        assert!(cover.is_none());
        let meta = compute_file_meta(&entry.source.path, session.chunk_size).expect("meta");
        let manifest = session.manifest_for(&entry, &meta, cover.as_deref(), None);
        assert_eq!(manifest.files.len(), 1);
        assert!(manifest.validate().is_ok(), "{:?}", manifest.validate());
    }

    #[tokio::test]
    async fn cover_png_sibling_rides_the_manifest_with_valid_hashes() {
        let (dir, path) = temp_audio(&[3u8; 500_000]);
        let cover_bytes = [9u8; 90_000];
        temp_cover(&dir, "cover.png", &cover_bytes);
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        let cover = session
            .ensure_cover_meta(&entry)
            .await
            .expect("cover hashes")
            .expect("cover present");
        assert_eq!(cover.wire, "cover.png");
        assert_eq!(cover.meta.size, cover_bytes.len() as u64);
        let meta = compute_file_meta(&entry.source.path, session.chunk_size).expect("meta");
        let manifest = session.manifest_for(&entry, &meta, Some(&cover), None);
        assert!(manifest.validate().is_ok(), "{:?}", manifest.validate());
        assert_eq!(manifest.files.len(), 2);
        let cover_entry = &manifest.files[1];
        assert_eq!(cover_entry.path, "cover.png");
        assert_eq!(cover_entry.size, cover_bytes.len() as u64);
        let expected = compute_file_meta(&cover.path, session.chunk_size).expect("cover meta");
        assert_eq!(cover_entry.sha256, expected.sha256);
        assert_eq!(cover_entry.chunk_hashes, expected.chunk_hashes);
    }

    #[tokio::test]
    async fn alignment_json_sibling_rides_manifest_and_populates_proto_meta() {
        let (dir, path) = temp_audio(&[3u8; 500_000]);
        let align_json =
            br#"{"origin":"measured","segments":[{"text":"hello","startMs":0,"endMs":500}]}"#;
        temp_file(&dir, "alignment.json", align_json);
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        let align = session
            .ensure_alignment_meta(&entry)
            .await
            .expect("alignment hashes")
            .expect("alignment present");
        assert_eq!(align.wire, "alignment.json");
        assert_eq!(align.meta.size, align_json.len() as u64);
        assert_eq!(align.proto_meta.origin.as_deref(), Some("measured"));
        assert_eq!(align.proto_meta.total_ms, Some(60_000));
        let meta = compute_file_meta(&entry.source.path, session.chunk_size).expect("meta");
        let manifest = session.manifest_for(&entry, &meta, None, Some(&align));
        assert!(manifest.validate().is_ok(), "{:?}", manifest.validate());
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.files[1].path, "alignment.json");
        assert_eq!(
            manifest
                .alignment
                .as_ref()
                .and_then(|a| a.origin.as_deref()),
            Some("measured")
        );
        assert_eq!(
            manifest.alignment.as_ref().and_then(|a| a.total_ms),
            Some(60_000)
        );
    }

    #[tokio::test]
    async fn cover_preference_is_png_then_jpg_then_webp() {
        let (dir, path) = temp_audio(&[3u8; 100_000]);
        temp_cover(&dir, "cover.webp", b"webp");
        temp_cover(&dir, "cover.jpg", b"jpg");
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        let no_png = session
            .ensure_cover_meta(&entry)
            .await
            .expect("cover hashes");
        assert_eq!(no_png.expect("cover present").wire, "cover.jpg");
        temp_cover(&dir, "cover.png", b"png");
        // A fresh session sees the new sibling (discovery runs per request).
        let session2 = session_for(entry.source.path.clone(), proto::MIN_CHUNK_SIZE as u64);
        let entry2 = session2.cast("cst_test").expect("entry");
        let with_png = session2
            .ensure_cover_meta(&entry2)
            .await
            .expect("cover hashes");
        assert_eq!(with_png.expect("cover present").wire, "cover.png");
    }

    #[tokio::test]
    async fn cover_removed_between_requests_is_absence_not_error() {
        let (dir, path) = temp_audio(&[3u8; 100_000]);
        let cover_path = temp_cover(&dir, "cover.jpg", b"here then gone");
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        assert!(session
            .ensure_cover_meta(&entry)
            .await
            .expect("cover hashes")
            .is_some());
        std::fs::remove_file(&cover_path).expect("remove cover");
        let gone = session
            .ensure_cover_meta(&entry)
            .await
            .expect("vanished cover is absence");
        assert!(gone.is_none());
    }

    #[test]
    fn qr_payload_matches_contract_shape() {
        let hosts = vec!["192.168.1.20:47317".to_string()];
        let payload = qr_payload(&hosts, "ab12", "cd34");
        let parsed: serde_json::Value = serde_json::from_str(&payload).expect("json");
        assert_eq!(parsed["v"], 1);
        assert_eq!(parsed["f"], "ab12");
        assert_eq!(parsed["h"][0], "192.168.1.20:47317");
        assert_eq!(parsed["s"], "cd34");
    }

    #[test]
    fn secret_is_single_use_and_locks_after_repeated_failures() {
        let (_dir, path) = temp_audio(b"x");
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let secret = session.mint_secret();
        assert!(matches!(
            session.try_pair("wrong", "Mallory"),
            PairOutcome::Rejected
        ));
        let token = match session.try_pair(&secret, "My Phone") {
            PairOutcome::Token(t) => t,
            PairOutcome::Rejected => panic!("correct secret rejected"),
        };
        assert!(session.token_valid(&token));
        assert!(matches!(
            session.try_pair(&secret, "Replay"),
            PairOutcome::Rejected
        ));
        assert!(!session.pair_locked());

        let secret = session.mint_secret();
        assert!(!session.pair_locked());
        for _ in 0..PAIR_FAILURE_LIMIT {
            assert!(matches!(
                session.try_pair("wrong", "Mallory"),
                PairOutcome::Rejected
            ));
        }
        assert!(session.pair_locked());
        assert!(matches!(
            session.try_pair(&secret, "Late"),
            PairOutcome::Rejected
        ));
    }

    #[test]
    fn secret_expires_at_exactly_five_minutes_on_the_desktop_clock() {
        let (_dir, path) = temp_audio(b"x");
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let t0 = Instant::now();
        let offset = Arc::new(Mutex::new(Duration::ZERO));
        let o = offset.clone();
        session.set_clock(Arc::new(move || t0 + *o.lock().unwrap()));
        let ttl = PAIR_SECRET_TTL;

        let almost = session.mint_secret();
        *offset.lock().unwrap() = ttl - Duration::from_millis(100);
        let token = match session.try_pair(&almost, "Phone") {
            PairOutcome::Token(t) => t,
            PairOutcome::Rejected => panic!("299.9 s: secret must still be valid"),
        };

        let expired = session.mint_secret(); // minted at 299.9 s
        *offset.lock().unwrap() = 2 * ttl - Duration::from_millis(100); // +300.0 s
        assert!(
            matches!(session.try_pair(&expired, "Late"), PairOutcome::Rejected),
            "300.0 s: secret must be expired"
        );
        for _ in 0..(PAIR_FAILURE_LIMIT * 2) {
            assert!(matches!(
                session.try_pair(&expired, "Late"),
                PairOutcome::Rejected
            ));
        }
        assert!(!session.pair_locked(), "expiry never counts toward lockout");
        assert!(
            session.token_valid(&token),
            "issued tokens outlive the secret"
        );

        let fresh = session.mint_secret(); // re-mint restarts the window
        assert!(matches!(
            session.try_pair(&fresh, "Phone"),
            PairOutcome::Token(_)
        ));
    }

    #[test]
    fn expiry_reads_the_clock_under_the_secret_lock_not_before() {
        // Review finding on #345: sampling `now` BEFORE taking the slot lock
        // lets a request that queued at 299.99 s be judged with that stale
        // reading after the lock is acquired past 300 s. This clock returns
        // 299.99 s while the slot is free and 300.01 s while it is held, so the
        // secret is accepted only if the clock is read outside the lock.
        let (_dir, path) = temp_audio(b"x");
        let session = Arc::new(session_for(path, proto::MIN_CHUNK_SIZE as u64));
        let t0 = Instant::now();
        let probe = session.clone();
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let armed_c = armed.clone();
        session.set_clock(Arc::new(move || {
            if !armed_c.load(Ordering::SeqCst) {
                return t0; // minting happens at t0
            }
            if probe.secret_slot_held() {
                t0 + PAIR_SECRET_TTL + Duration::from_millis(10)
            } else {
                t0 + PAIR_SECRET_TTL - Duration::from_millis(10)
            }
        }));
        let secret = session.mint_secret();
        armed.store(true, Ordering::SeqCst);
        assert!(
            matches!(session.try_pair(&secret, "Queued"), PairOutcome::Rejected),
            "clock must be sampled under the lock: 300.01 s there, so rejected"
        );
        assert!(!session.pair_locked());
    }

    #[test]
    fn consumed_secret_replay_is_refused_without_counting_toward_lockout() {
        let (_dir, path) = temp_audio(b"x");
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let secret = session.mint_secret();
        let token = match session.try_pair(&secret, "My Phone") {
            PairOutcome::Token(t) => t,
            PairOutcome::Rejected => panic!("correct secret rejected"),
        };
        assert!(session.token_valid(&token));
        for _ in 0..(PAIR_FAILURE_LIMIT * 3) {
            assert!(matches!(
                session.try_pair(&secret, "Stale APK"),
                PairOutcome::Rejected
            ));
        }
        assert!(!session.pair_locked());
        for _ in 0..PAIR_FAILURE_LIMIT {
            assert!(matches!(
                session.try_pair("wrong", "Mallory"),
                PairOutcome::Rejected
            ));
        }
        assert!(session.pair_locked());
    }

    #[test]
    fn consumed_secrets_stay_exempt_across_fresh_mints() {
        let (_dir, path) = temp_audio(b"x");
        let session = session_for(path, proto::MIN_CHUNK_SIZE as u64);
        let first = session.mint_secret();
        assert!(matches!(
            session.try_pair(&first, "Phone"),
            PairOutcome::Token(_)
        ));
        let second = session.mint_secret();
        for _ in 0..(PAIR_FAILURE_LIMIT * 2) {
            assert!(matches!(
                session.try_pair(&first, "Stale APK"),
                PairOutcome::Rejected
            ));
        }
        assert!(!session.pair_locked());
        assert!(matches!(
            session.try_pair(&second, "New Phone"),
            PairOutcome::Token(_)
        ));
        assert!(matches!(
            session.try_pair("wrong", "Mallory"),
            PairOutcome::Rejected
        ));
        assert_eq!(
            {
                let slot = session.secret.lock().expect("secret slot poisoned");
                slot.failures
            },
            1
        );
    }

    #[test]
    fn peer_gate_admits_lan_and_rejects_public() {
        let allowed = [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.3.4",
            "192.168.1.9",
            "169.254.10.10",
            "::1",
            "fd12:3456::1",
            "fe80::abcd",
            "::ffff:192.168.1.9",
        ];
        for ip in allowed {
            assert!(peer_allowed(&ip.parse().unwrap()), "{ip} should be allowed");
        }
        let rejected = [
            "8.8.8.8",
            "1.1.1.1",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
        ];
        for ip in rejected {
            assert!(
                !peer_allowed(&ip.parse().unwrap()),
                "{ip} should be rejected"
            );
        }
    }

    #[test]
    fn advertised_hosts_prefer_private_v4_and_skip_public_and_link_local() {
        let interfaces: Vec<IpAddr> = vec![
            "8.8.8.8".parse().unwrap(),
            "fe80::1".parse().unwrap(),
            "fd00::2".parse().unwrap(),
            "192.168.1.20".parse().unwrap(),
            "192.168.1.20".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        ];
        let hosts = advertised_hosts(&interfaces, 4000);
        assert_eq!(hosts, vec!["192.168.1.20:4000", "[fd00::2]:4000"]);
    }

    fn session_err(result: Result<SessionState, String>) -> String {
        match result {
            Ok(_) => panic!("session unexpectedly accepted"),
            Err(err) => err,
        }
    }

    #[test]
    fn unsafe_cast_ids_are_refused_and_titles_sanitized() {
        let (_dir, path) = temp_audio(b"x");
        let err = session_err(SessionState::new(
            vec![CastSource {
                cast_id: "../escape".into(),
                title: "A Book".into(),
                path: path.clone(),
                cover_path: None,
                alignment_path: None,
                duration_seconds: None,
            }],
            proto::MIN_CHUNK_SIZE as u64,
            noop_sink(),
        ));
        assert!(err.contains("unsafe shape"));

        let session = SessionState::new(
            vec![CastSource {
                cast_id: "cst_ok".into(),
                title: "  Ctrl\u{0}Title \n".into(),
                path,
                cover_path: None,
                alignment_path: None,
                duration_seconds: None,
            }],
            proto::MIN_CHUNK_SIZE as u64,
            noop_sink(),
        )
        .expect("session");
        assert_eq!(session.casts[0].source.title, "CtrlTitle");
    }

    #[test]
    fn non_audio_sources_are_refused_at_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello").expect("write");
        let err = session_err(SessionState::new(
            vec![CastSource {
                cast_id: "cst_bad".into(),
                title: "Notes".into(),
                path,
                cover_path: None,
                alignment_path: None,
                duration_seconds: None,
            }],
            proto::MIN_CHUNK_SIZE as u64,
            noop_sink(),
        ));
        assert!(err.contains("not a sendable audio file"));
    }

    #[tokio::test]
    async fn explicit_cover_and_alignment_paths_are_served() {
        let (_dir, audio_path) = temp_audio(&[3u8; 100_000]);
        let cover_dir = tempfile::tempdir().expect("cover tempdir");
        let cover_path = cover_dir.path().join("my_cover.png");
        std::fs::write(&cover_path, b"png_data").expect("write cover");

        let align_dir = tempfile::tempdir().expect("align tempdir");
        let align_path = align_dir.path().join("my_alignment.json");
        std::fs::write(
            &align_path,
            br#"{"origin":"dynamic","totalMs":5000,"segments":[]}"#,
        )
        .expect("write align");

        let session = SessionState::new(
            vec![CastSource {
                cast_id: "cst_explicit".into(),
                title: "Explicit Book".into(),
                path: audio_path,
                cover_path: Some(cover_path),
                alignment_path: Some(align_path),
                duration_seconds: None,
            }],
            proto::MIN_CHUNK_SIZE as u64,
            noop_sink(),
        )
        .expect("session");

        let entry = session.cast("cst_explicit").expect("entry");
        let cover = session
            .ensure_cover_meta(&entry)
            .await
            .expect("cover meta")
            .expect("cover present");
        assert_eq!(cover.wire, "cover.png");

        let align = session
            .ensure_alignment_meta(&entry)
            .await
            .expect("align meta")
            .expect("align present");
        assert_eq!(align.wire, "alignment.json");
        assert_eq!(align.proto_meta.total_ms, Some(5000));
        assert_eq!(align.proto_meta.origin.as_deref(), Some("dynamic"));

        let meta = compute_file_meta(&entry.source.path, session.chunk_size).expect("meta");
        let manifest = session.manifest_for(&entry, &meta, Some(&cover), Some(&align));
        assert!(manifest.validate().is_ok(), "{:?}", manifest.validate());
        assert_eq!(manifest.files.len(), 3);
        assert_eq!(manifest.files[0].path, "audio.mp3");
        assert_eq!(manifest.files[1].path, "cover.png");
        assert_eq!(manifest.files[2].path, "alignment.json");
        assert_eq!(
            manifest.alignment.as_ref().unwrap().origin.as_deref(),
            Some("dynamic")
        );
        assert_eq!(manifest.alignment.as_ref().unwrap().total_ms, Some(5000));
    }

    #[tokio::test]
    async fn ensure_meta_detects_changed_bytes_and_updates_etag() {
        let (_dir, path) = temp_audio(&[1u8; 200_000]);
        let session = session_for(path.clone(), proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        let first = session.ensure_meta(&entry).await.expect("meta");
        std::fs::write(&path, [2u8; 300_000]).expect("rewrite");
        let second = session.ensure_meta(&entry).await.expect("meta");
        assert_ne!(first.etag, second.etag);
        assert_eq!(second.size, 300_000);
    }

    #[tokio::test]
    async fn receipt_marks_sent_complete_and_flips_phase_to_done() {
        let (_dir, path) = temp_audio(&[4u8; 100_000]);
        let session = session_for(path.clone(), proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        let meta = session.ensure_meta(&entry).await.expect("meta");

        let secret = session.mint_secret();
        session.try_pair(&secret, "Phone");
        assert_eq!(session.snapshot().phase, "paired");

        let receipt = proto::Receipt {
            cast_id: "cst_test".into(),
            files: vec![proto::ReceiptFile {
                path: "audio.mp3".into(),
                sha256: meta.sha256.clone(),
                action: "reused".into(),
            }],
        };

        let ok = session
            .record_receipt(&receipt)
            .await
            .expect("record receipt");
        assert!(ok);
        assert!(entry.sent_complete.load(Ordering::Relaxed));
        assert_eq!(session.snapshot().phase, "done");
    }

    #[tokio::test]
    async fn record_receipt_is_idempotent_pinned_by_double_call() {
        let (_dir, path) = temp_audio(&[5u8; 100_000]);
        let session = session_for(path.clone(), proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        let meta = session.ensure_meta(&entry).await.expect("meta");

        let secret = session.mint_secret();
        session.try_pair(&secret, "Phone");

        let receipt = proto::Receipt {
            cast_id: "cst_test".into(),
            files: vec![proto::ReceiptFile {
                path: "audio.mp3".into(),
                sha256: meta.sha256.clone(),
                action: "received".into(),
            }],
        };

        assert!(session.record_receipt(&receipt).await.is_ok());
        assert_eq!(session.snapshot().phase, "done");
        assert!(entry.sent_complete.load(Ordering::Relaxed));

        assert!(session.record_receipt(&receipt).await.is_ok());
        assert_eq!(session.snapshot().phase, "done");
        assert!(entry.sent_complete.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn record_receipt_rejects_unknown_cast_or_mismatched_sha() {
        let (_dir, path) = temp_audio(&[6u8; 100_000]);
        let session = session_for(path.clone(), proto::MIN_CHUNK_SIZE as u64);
        let entry = session.cast("cst_test").expect("entry");
        let meta = session.ensure_meta(&entry).await.expect("meta");

        let unknown_cast = proto::Receipt {
            cast_id: "cst_unknown".into(),
            files: vec![proto::ReceiptFile {
                path: "audio.mp3".into(),
                sha256: meta.sha256.clone(),
                action: "received".into(),
            }],
        };
        assert!(session.record_receipt(&unknown_cast).await.is_err());

        let bad_hash = proto::Receipt {
            cast_id: "cst_test".into(),
            files: vec![proto::ReceiptFile {
                path: "audio.mp3".into(),
                sha256: "0".repeat(64),
                action: "received".into(),
            }],
        };
        assert!(session.record_receipt(&bad_hash).await.is_err());
    }
}
