use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use sqlx::SqlitePool;

use crate::orchestrator::terminal_states_csv;
use crate::render::mp3::cache_wav_path;
use crate::storage::{cache, job, settings};

pub const CAP_SETTING_KEY: &str = "cache.cap_bytes";
pub const DEFAULT_CAP_BYTES: i64 = 20 * 1024 * 1024 * 1024;
pub const PREVIEW_DIR: &str = "previews";
pub const PASS_BUDGET: Duration = Duration::from_secs(2);
pub const PREVIEW_GRACE: Duration = Duration::from_secs(300);
const EVICTION_BATCH: i64 = 256;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvictionOutcome {
    pub cap_bytes: Option<i64>,
    pub bytes_before: i64,
    pub bytes_after: i64,
    pub evicted: u64,
    pub bytes_freed: i64,
    pub unlink_failed: u64,
    pub pinned_bytes: i64,
    pub previews_removed: u64,
    pub preview_bytes_freed: i64,
    pub temps_removed: u64,
    pub temp_bytes_freed: i64,
    pub budget_exhausted: bool,
}

impl EvictionOutcome {
    pub fn over_cap(&self) -> bool {
        matches!(self.cap_bytes, Some(cap) if self.bytes_after > cap)
    }

    pub fn is_reportable(&self) -> bool {
        self.evicted > 0
            || self.unlink_failed > 0
            || self.previews_removed > 0
            || self.temps_removed > 0
            || self.over_cap()
    }

    pub fn accumulated_onto(&self, previous: Option<&serde_json::Value>) -> serde_json::Value {
        let carried = |key: &str| -> u64 {
            previous
                .and_then(|block| block.get(key))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        json!({
            "cap_bytes": self.cap_bytes,
            "bytes_before": previous
                .and_then(|block| block.get("bytes_before"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(self.bytes_before),
            "bytes_after": self.bytes_after,
            "evicted": carried("evicted") + self.evicted,
            "bytes_freed": carried("bytes_freed") as i64 + self.bytes_freed,
            "unlink_failed": self.unlink_failed,
            "pinned_bytes": self.pinned_bytes,
            "previews_removed": carried("previews_removed") + self.previews_removed,
            "temps_removed": carried("temps_removed") + self.temps_removed,
            "temp_bytes_freed": carried("temp_bytes_freed") as i64 + self.temp_bytes_freed,
            "preview_bytes_freed": carried("preview_bytes_freed") as i64 + self.preview_bytes_freed,
            "passes": carried("passes") + 1,
            "over_cap": self.over_cap(),
            "budget_exhausted": self.budget_exhausted,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapSource {
    Default,
    Unlimited,
    Configured(i64),
    Refused { raw: String },
}

impl CapSource {
    pub fn effective_bytes(&self) -> Option<i64> {
        match self {
            Self::Default | Self::Refused { .. } => Some(DEFAULT_CAP_BYTES),
            Self::Unlimited => None,
            Self::Configured(cap) => Some(*cap),
        }
    }
}

pub fn classify_cap(raw: Option<&str>) -> CapSource {
    let Some(raw) = raw else {
        return CapSource::Default;
    };
    match raw.trim().parse::<i64>() {
        Ok(cap) if cap > 0 => CapSource::Configured(cap),
        Ok(_) => CapSource::Unlimited,
        Err(_) => CapSource::Refused {
            raw: raw.to_string(),
        },
    }
}

pub async fn read_cap(pool: &SqlitePool) -> Result<CapSource, sqlx::Error> {
    let raw = settings::get_setting(pool, CAP_SETTING_KEY).await?;
    Ok(classify_cap(raw.as_deref()))
}

pub async fn read_cap_bytes(pool: &SqlitePool) -> Result<Option<i64>, sqlx::Error> {
    Ok(read_cap(pool).await?.effective_bytes())
}

pub async fn write_cap_bytes(pool: &SqlitePool, cap_bytes: i64) -> Result<(), String> {
    if cap_bytes < 0 {
        return Err(format!(
            "a cache cap cannot be negative; {cap_bytes} would be read as no limit"
        ));
    }
    settings::set_setting(pool, CAP_SETTING_KEY, &cap_bytes.to_string())
        .await
        .map_err(|e| e.to_string())
}

pub async fn clear_cap(pool: &SqlitePool) -> Result<(), String> {
    settings::delete_setting(pool, CAP_SETTING_KEY)
        .await
        .map_err(|e| e.to_string())
}

pub async fn enforce_cap(
    pool: &SqlitePool,
    cache_dir: &Path,
) -> Result<EvictionOutcome, sqlx::Error> {
    let started = Instant::now();
    let cap = read_cap_bytes(pool).await?;
    let mut total = cache::total_bytes(pool).await?;
    let mut outcome = EvictionOutcome {
        cap_bytes: cap,
        bytes_before: total,
        bytes_after: total,
        ..EvictionOutcome::default()
    };

    if let Some(cap) = cap {
        if total > cap {
            let releasing = terminal_states_csv();
            outcome.pinned_bytes = cache::pinned_bytes(pool, &releasing).await?;
            let mut unlink_failures: HashSet<String> = HashSet::new();
            let deadline = Instant::now() + PASS_BUDGET;
            while total > cap {
                if Instant::now() >= deadline {
                    outcome.budget_exhausted = true;
                    break;
                }
                let batch = cache::lru_evictable(pool, EVICTION_BATCH, &releasing).await?;
                let mut progressed = false;
                for entry in batch {
                    if total <= cap {
                        break;
                    }
                    if Instant::now() >= deadline {
                        outcome.budget_exhausted = true;
                        break;
                    }
                    if unlink_failures.contains(&entry.hash) {
                        continue;
                    }
                    if unlink_artifact(evictable_path(cache_dir, &entry.hash).as_deref()).await {
                        cache::remove(pool, &entry.hash).await?;
                        total -= entry.bytes;
                        outcome.bytes_freed += entry.bytes;
                        outcome.evicted += 1;
                        progressed = true;
                    } else {
                        unlink_failures.insert(entry.hash);
                    }
                }
                if !progressed {
                    tracing::warn!(
                        "cache eviction stalled {} bytes over cap: every evictable entry \
                         in the batch failed to unlink",
                        total - cap
                    );
                    break;
                }
            }
            if outcome.budget_exhausted {
                tracing::warn!(
                    "cache eviction pass hit its time budget with {} bytes still over cap",
                    total.saturating_sub(cap).max(0)
                );
            }
            outcome.unlink_failed = unlink_failures.len() as u64;
        }
    }

    let (temps_removed, temp_bytes_freed, temps_budget_exhausted) =
        sweep_temp_writes(cache_dir, started + PASS_BUDGET).await;
    outcome.temps_removed = temps_removed;
    outcome.temp_bytes_freed = temp_bytes_freed;
    outcome.budget_exhausted = outcome.budget_exhausted || temps_budget_exhausted;
    let (previews_removed, preview_bytes_freed) = sweep_previews(pool, cache_dir).await?;
    outcome.previews_removed = previews_removed;
    outcome.preview_bytes_freed = preview_bytes_freed;
    outcome.bytes_after = cache::total_bytes(pool).await?;
    Ok(outcome)
}

pub async fn enforce_cap_for_job(
    pool: &SqlitePool,
    cache_dir: &Path,
    job_id: &str,
) -> Result<EvictionOutcome, sqlx::Error> {
    let outcome = enforce_cap(pool, cache_dir).await?;
    if outcome.is_reportable() {
        let previous = job::get_job(pool, job_id)
            .await?
            .and_then(|record| record.report_json)
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|report| report.get("cache_eviction").cloned())
            .filter(serde_json::Value::is_object);
        let block = outcome.accumulated_onto(previous.as_ref());
        job::merge_report_value(pool, job_id, "cache_eviction", block).await?;
    }
    Ok(outcome)
}

pub fn evictable_path(cache_dir: &Path, hash: &str) -> Option<PathBuf> {
    if hash.len() < 2 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        tracing::warn!("cache eviction refused a malformed artifact key '{hash}'");
        return None;
    }
    let path = cache_wav_path(cache_dir, hash);
    let shard = path.parent()?;
    match (shard.canonicalize(), cache_dir.canonicalize()) {
        (Ok(resolved_shard), Ok(root)) if !resolved_shard.starts_with(&root) => {
            tracing::warn!(
                "cache eviction refused {} outside {}",
                resolved_shard.display(),
                root.display()
            );
            None
        }
        _ => Some(path),
    }
}

pub const TEMP_WRITE_PREFIX: &str = ".tmp-";

fn is_partial_encode(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && ext.strip_prefix("partial").is_some_and(|rest| {
                    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
                })
        }
        None => false,
    }
}

fn shard_within_cache(cache_dir: &Path, shard: &Path) -> bool {
    let name = shard.file_name().and_then(|value| value.to_str());
    let Some(name) = name else {
        return false;
    };
    if name.len() != 2 || !name.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    match (shard.canonicalize(), cache_dir.canonicalize()) {
        (Ok(resolved), Ok(root)) => resolved.starts_with(&root),
        _ => false,
    }
}

async fn sweep_temp_writes(cache_dir: &Path, deadline: Instant) -> (u64, i64, bool) {
    let Ok(shards) = std::fs::read_dir(cache_dir) else {
        return (0, 0, false);
    };
    let mut removed = 0;
    let mut freed = 0;
    for shard in shards.flatten() {
        if Instant::now() >= deadline {
            return (removed, freed, true);
        }
        let shard_path = shard.path();
        if !shard_path.is_dir() || !shard_within_cache(cache_dir, &shard_path) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&shard_path) else {
            continue;
        };
        for entry in entries.flatten() {
            if Instant::now() >= deadline {
                return (removed, freed, true);
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !name.starts_with(TEMP_WRITE_PREFIX) && !is_partial_encode(name) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if written_within_grace(&meta) {
                continue;
            }
            let bytes = meta.len() as i64;
            if unlink_artifact(Some(&path)).await {
                removed += 1;
                freed += bytes;
            }
        }
    }
    (removed, freed, false)
}

async fn unlink_artifact(path: Option<&Path>) -> bool {
    let Some(path) = path else {
        return false;
    };
    match tokio::fs::remove_file(path).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            let gone = matches!(path.try_exists(), Ok(false));
            if !gone {
                tracing::warn!(
                    "cache eviction could not unlink {}: {error}",
                    path.display()
                );
            }
            gone
        }
    }
}

async fn sweep_previews(pool: &SqlitePool, cache_dir: &Path) -> Result<(u64, i64), sqlx::Error> {
    let preview_dir = cache_dir.join(PREVIEW_DIR);
    let Ok(entries) = std::fs::read_dir(&preview_dir) else {
        return Ok((0, 0));
    };
    let mut removed = 0;
    let mut freed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("wav") {
            continue;
        }
        let Some(hash) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if cache::get_entry(pool, hash).await?.is_some() {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if written_within_grace(&meta) {
            continue;
        }
        let bytes = meta.len() as i64;
        if unlink_artifact(Some(&path)).await {
            removed += 1;
            freed += bytes;
        }
    }
    Ok((removed, freed))
}

fn written_within_grace(meta: &std::fs::Metadata) -> bool {
    meta.modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age < PREVIEW_GRACE)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_encode_is_the_indexed_scratch_name_the_chapter_encoder_writes() {
        assert!(is_partial_encode("aa11.partial0"));
        assert!(is_partial_encode("bb22.partial12"));
    }

    #[test]
    fn a_bare_or_wrongly_suffixed_partial_word_is_not_a_chapter_scratch_file() {
        for name in [
            "aa11.partial",
            "aa11.partial_backup",
            "aa11.partial0.tmp",
            "partial0",
            ".partial0",
            "aa11.mp3.partial",
        ] {
            assert!(
                !is_partial_encode(name),
                "{name} is not a name this module writes, so sweeping it would eat a real file"
            );
        }
    }

    #[test]
    fn an_unlimited_cap_is_never_over_cap() {
        assert!(EvictionOutcome {
            cap_bytes: Some(10),
            bytes_after: 11,
            ..EvictionOutcome::default()
        }
        .over_cap());
        assert!(!EvictionOutcome {
            cap_bytes: Some(10),
            bytes_after: 10,
            ..EvictionOutcome::default()
        }
        .over_cap());
        assert!(!EvictionOutcome {
            cap_bytes: None,
            bytes_after: i64::MAX,
            ..EvictionOutcome::default()
        }
        .over_cap());
    }

    #[test]
    fn a_quiet_pass_is_not_worth_a_report_block() {
        assert!(!EvictionOutcome {
            cap_bytes: Some(100),
            bytes_before: 10,
            bytes_after: 10,
            ..EvictionOutcome::default()
        }
        .is_reportable());
        assert!(EvictionOutcome {
            cap_bytes: Some(100),
            bytes_before: 10,
            bytes_after: 10,
            evicted: 1,
            ..EvictionOutcome::default()
        }
        .is_reportable());
        assert!(EvictionOutcome {
            cap_bytes: Some(100),
            bytes_before: 10,
            bytes_after: 10,
            unlink_failed: 1,
            ..EvictionOutcome::default()
        }
        .is_reportable());
        assert!(EvictionOutcome {
            cap_bytes: Some(100),
            bytes_before: 10,
            bytes_after: 10,
            previews_removed: 1,
            ..EvictionOutcome::default()
        }
        .is_reportable());
    }

    #[test]
    fn the_cap_setting_key_has_one_definition_in_the_tree() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders: Vec<String> = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let relative = path.strip_prefix(&root).expect("under src").to_path_buf();
                let body = std::fs::read_to_string(&path).expect("read source");
                for (number, line) in body.lines().enumerate() {
                    if !line.contains("\"cache.cap_bytes\"") {
                        continue;
                    }
                    if relative == std::path::Path::new("cache_gc.rs")
                        && line.contains("CAP_SETTING_KEY: &str")
                    {
                        continue;
                    }
                    offenders.push(format!("{}:{}", relative.display(), number + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the cache cap key is defined once as cache_gc::CAP_SETTING_KEY; \
             these spell the literal themselves: {offenders:?}"
        );
    }
}
