//! Ingestion-completion index lifecycle: build a book's retrieval index from
//! its real `script_segment` rows once scripting has committed them, and
//! lazily rebuild via `ensure_indexed` for libraries that predate the index.
//!
//! Runtime-bound SQL (no `query!` macros) keeps the `.sqlx` offline cache
//! untouched. The index lives under `books/<book_id>/index/`, inside the
//! book's own data folder, so the existing `delete_book` directory removal
//! cleans it with the book by construction; `delete_index` covers the
//! re-ingest/invalidation path.
//!
//! Provenance projection (charter §3.2, ambiguity A1 safe default): only
//! unedited Narrator-mode `text`/`heading` rows are classed source-faithful.
//! Every Teacher-mode row (all LLM explanation), plus `chapter_intro`,
//! `figure_cue`, and any row whose `original_text` records an edit, is
//! generated retrieval metadata and is never returned as an evidence window.
//! The full imported-source span resolver is a named follow-up before any
//! whole-book-coverage claim.

use std::sync::Arc;

use sqlx::{Row, SqlitePool};

use super::provider::{
    BookFingerprint, IndexStats, Provenance, RetrievalError, RetrievalProvider, SegmentRow,
};
use super::tantivy_provider::TantivyRetrieval;

#[derive(Debug)]
pub enum HookError {
    /// Job or its book row is missing.
    UnknownJob,
    /// Database failure (bounded; carries no SQL).
    Db,
    Retrieval(RetrievalError),
}

impl std::fmt::Display for HookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownJob => write!(f, "unknown_job"),
            Self::Db => write!(f, "retrieval_db"),
            Self::Retrieval(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for HookError {}

impl From<RetrievalError> for HookError {
    fn from(e: RetrievalError) -> Self {
        Self::Retrieval(e)
    }
}

/// Resolve the active generation fingerprint for a render job.
pub async fn fingerprint_for_job(
    pool: &SqlitePool,
    job_id: &str,
) -> Result<BookFingerprint, HookError> {
    let row = sqlx::query(
        "SELECT b.id AS book_id, b.content_hash, COALESCE(b.blocks_version, '') AS blocks_version
         FROM render_job rj JOIN book b ON b.id = rj.book_id WHERE rj.id = ?",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| HookError::Db)?
    .ok_or(HookError::UnknownJob)?;
    let book_id: String = row.get("book_id");
    let content_hash: String = row.get("content_hash");
    let blocks_version: String = row.get("blocks_version");
    Ok(BookFingerprint::new(
        &book_id,
        job_id,
        &content_hash,
        &blocks_version,
    ))
}

/// Project the job's committed `script_segment` rows into retrieval children.
///
/// Provenance is mode-aware: in a Teacher render every `text` row is LLM
/// explanation, so the whole job is classed Generated regardless of `kind` —
/// retrievable metadata, never evidence (T9). Only unedited Narrator
/// `text`/`heading` rows are classed source-faithful under the A1 safe
/// default; the span-level source resolver remains the named follow-up.
async fn load_rows(pool: &SqlitePool, job_id: &str) -> Result<Vec<SegmentRow>, HookError> {
    let rows = sqlx::query(
        "SELECT c.idx AS chapter_idx, s.page_no, s.idx, s.kind, s.text,
                (s.original_text IS NOT NULL) AS edited, rj.mode
         FROM script_segment s
         JOIN chapter c ON c.id = s.chapter_id
         JOIN render_job rj ON rj.id = s.job_id
         WHERE s.job_id = ? ORDER BY s.idx",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await
    .map_err(|_| HookError::Db)?;
    let to_u32 = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
    Ok(rows
        .into_iter()
        .map(|r| {
            let kind: String = r.get("kind");
            let edited: bool = r.get("edited");
            let mode: String = r.get("mode");
            let narrator_faithful =
                mode == "narrator" && !edited && matches!(kind.as_str(), "text" | "heading");
            let provenance = if narrator_faithful {
                Provenance::SourceFaithful
            } else {
                Provenance::Generated
            };
            SegmentRow {
                chapter: to_u32(r.get::<i64, _>("chapter_idx")),
                page: to_u32(r.get::<i64, _>("page_no")),
                idx: to_u32(r.get::<i64, _>("idx")),
                text: r.get("text"),
                provenance,
            }
        })
        .collect())
}

/// Build (or rebuild) the index for a completed scripting job and publish it
/// atomically. The tantivy build runs on the blocking pool.
pub async fn build_for_job(
    pool: &SqlitePool,
    provider: Arc<TantivyRetrieval>,
    job_id: &str,
) -> Result<(BookFingerprint, IndexStats), HookError> {
    let fingerprint = fingerprint_for_job(pool, job_id).await?;
    let rows = load_rows(pool, job_id).await?;
    let fp = fingerprint.clone();
    let stats = tokio::task::spawn_blocking(move || provider.index_book(&fp, &rows))
        .await
        .map_err(|_| HookError::Retrieval(RetrievalError::Backend { code: "join" }))??;
    Ok((fingerprint, stats))
}

/// Lazy path for existing libraries: build only when no fresh index exists.
/// Returns the fingerprint and `Some(stats)` when a build ran.
pub async fn ensure_indexed(
    pool: &SqlitePool,
    provider: Arc<TantivyRetrieval>,
    job_id: &str,
) -> Result<(BookFingerprint, Option<IndexStats>), HookError> {
    let fingerprint = fingerprint_for_job(pool, job_id).await?;
    if provider.is_indexed(&fingerprint) {
        return Ok((fingerprint, None));
    }
    let (fp, stats) = build_for_job(pool, provider, job_id).await?;
    Ok((fp, Some(stats)))
}
