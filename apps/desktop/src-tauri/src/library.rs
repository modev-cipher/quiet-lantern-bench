use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::ingest_fallback::{FallbackBlock, FallbackSource};
use crate::orchestrator::stage::{ControlReceiver, StageOutcome};
use crate::orchestrator::stream::{post_sse_cancellable, SidecarTarget, StreamOutcome};
use crate::sidecar::client::{post_sse, SseEvent};
use crate::storage::{book, models, new_id, now_ms};

#[derive(Debug, Clone, Serialize)]
struct IngestRequest {
    book_path: String,
    book_id: String,
    docling_artifacts_dir: Option<String>,
    docling_artifacts_sha: Option<String>,
    layout: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback: Option<FallbackBlock>,
}

impl IngestRequest {
    async fn build(
        pool: &SqlitePool,
        book_path: String,
        book_id: String,
        docling_artifacts_dir: Option<String>,
        docling_artifacts_sha: Option<String>,
        layout: bool,
        fallback: &FallbackSource,
    ) -> Self {
        let fallback = fallback
            .resolve(pool)
            .await
            .map(|block| block.readings_beside(Path::new(&book_path)));
        IngestRequest {
            book_path,
            book_id,
            docling_artifacts_dir,
            docling_artifacts_sha,
            layout,
            fallback,
        }
    }
}

pub const DOCLING_LAYOUT_MODEL_ID: &str = "docling-layout";

pub async fn docling_artifacts_sha(pool: &SqlitePool) -> Option<String> {
    models::get_model_install(pool, DOCLING_LAYOUT_MODEL_ID)
        .await
        .ok()
        .flatten()
        .map(|install| install.sha256)
}

pub fn docling_artifacts_dir(models_root: &Path) -> Option<String> {
    let dir = models_root.join("layout").join("docling-layout");
    dir.is_dir().then(|| dir.to_string_lossy().into_owned())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BookSummary {
    pub format: String,
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    pub page_count: Option<i64>,
    pub chapter_count: i64,
    pub language: Option<String>,
    pub is_scanned: bool,
    pub cover_path: Option<String>,
    pub created_at: i64,
    pub has_pseudo_chapters: bool,
    pub layout_analysed: bool,
    pub reflowed: bool,
    /// This book's stored blocks predate the extractor this build ships.
    pub blocks_stale: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub book: BookSummary,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ImportEvent {
    Progress {
        page: i64,
        total: i64,
        #[serde(default = "default_import_stage")]
        stage: String,
    },
    Done {
        book_id: String,
    },
}

fn default_import_stage() -> String {
    "extract".to_string()
}

#[derive(Debug)]
enum Outcome {
    Manifest(serde_json::Value),
    Cancelled,
    Error { code: String, message: String },
}

/// Is this book's stored generation the one this build would produce?
///
/// `book.blocks_version` was written at ingest and never compared against anything, so a
/// bumped extractor invalidated nothing: the library still holds books at `extract=1`, and
/// `ERR_ALREADY_ANALYSED` refused to re-read exactly the books whose labels were oldest.
/// The comparison is against the engine's **base** identity (`/health.extract_version`),
/// not the whole stored string, because the stored one appends `|docling=<sha>` — the hash
/// of the *installed* artifacts, which would mark every book stale whenever a model is
/// re-downloaded, which is not what this question asks.
///
/// `|docling=<sha>` is the **only** sanctioned suffix, and this function is why: it accepts
/// the base plus anything after a `|`, so a second suffix added to
/// `ingest/version.extractor_version` would be silently tolerated here and would stop being
/// able to invalidate a book. A new component belongs in the base that both ends compare, or
/// it needs its own compare — never a third field bolted onto the stored string.
///
/// An engine that cannot answer means "current": staleness has to be proven before it can
/// cost the user a multi-minute pass.
pub fn generation_is_current(stored: Option<&str>, expected: Option<&str>) -> bool {
    let Some(expected) = expected.filter(|value| !value.is_empty()) else {
        return true;
    };
    let Some(stored) = stored else {
        return false;
    };
    stored == expected || stored.starts_with(&format!("{expected}|"))
}

pub async fn summarize(
    pool: &SqlitePool,
    record: &book::Book,
    expected_generation: Option<&str>,
) -> Result<BookSummary, String> {
    let chapters = book::list_chapters(pool, &record.id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(BookSummary {
        format: record.format.clone(),
        id: record.id.clone(),
        title: record.title.clone(),
        author: record.author.clone(),
        page_count: record.page_count,
        chapter_count: chapters.len() as i64,
        language: record.language.clone(),
        is_scanned: record.is_scanned,
        cover_path: record.cover_path.clone(),
        created_at: record.created_at,
        has_pseudo_chapters: chapters.iter().any(|c| c.is_pseudo),
        layout_analysed: record.layout_analysed,
        reflowed: record.reflowed,
        blocks_stale: !generation_is_current(record.blocks_version.as_deref(), expected_generation),
    })
}

pub async fn hash_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("cannot open '{}': {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn source_extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "pdf".into())
}

fn title_fallback(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "Untitled".into())
}

fn build_pages(book_id: &str, page_count: i64, rows: &[serde_json::Value]) -> Vec<book::Page> {
    let mut by_page: std::collections::HashMap<i64, &serde_json::Value> =
        std::collections::HashMap::new();
    for row in rows {
        if let Some(page_no) = row["page_no"].as_i64() {
            by_page.insert(page_no, row);
        }
    }
    (1..=page_count)
        .map(|page_no| {
            let row = by_page.get(&page_no);
            book::Page {
                id: new_id(),
                book_id: book_id.to_string(),
                page_no,
                width_pt: row
                    .and_then(|r| r["width_pt"].as_f64())
                    .filter(|v| *v > 0.0),
                height_pt: row
                    .and_then(|r| r["height_pt"].as_f64())
                    .filter(|v| *v > 0.0),
                render_hash: None,
                text_json: row
                    .and_then(|r| r["text_json"].as_str())
                    .map(|text| text.to_string()),
                needs_upgrade: row
                    .and_then(|r| r["needs_upgrade"].as_bool())
                    .unwrap_or(false),
            }
        })
        .collect()
}

fn manifest_str(manifest: &serde_json::Value, key: &str) -> Option<String> {
    manifest[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Some PDF producers ship the metadata field's own name as its value
/// ("Title", "Author"); such a value carries no information and must not
/// shadow the filename fallback.
fn manifest_meta(manifest: &serde_json::Value, key: &str) -> Option<String> {
    manifest_str(manifest, key).filter(|value| !value.eq_ignore_ascii_case(key))
}

#[allow(clippy::too_many_arguments)]
pub async fn import_book_with_control<F>(
    pool: &SqlitePool,
    client: &reqwest::Client,
    port: u16,
    token: &str,
    books_dir: &Path,
    source: &Path,
    docling_dir: Option<String>,
    layout: bool,
    fallback: &FallbackSource,
    control: Option<&mut ControlReceiver>,
    mut on_event: F,
) -> Result<ImportResult, String>
where
    F: FnMut(ImportEvent),
{
    let content_hash = hash_file(source).await?;
    if let Some(existing) = book::find_book_by_content_hash(pool, &content_hash)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "ERR_DUPLICATE: '{}' is already in your library",
            existing.title
        ));
    }

    let book_id = new_id();
    let book_dir = books_dir.join(&book_id);
    let result = ingest_into(
        pool,
        client,
        port,
        token,
        &book_dir,
        source,
        &book_id,
        &content_hash,
        docling_dir,
        docling_artifacts_sha(pool).await,
        layout,
        fallback,
        control,
        &mut on_event,
    )
    .await;
    if result.is_err() {
        let _ = book::delete_book(pool, &book_id).await;
        let _ = tokio::fs::remove_dir_all(&book_dir).await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub async fn import_book<F>(
    pool: &SqlitePool,
    client: &reqwest::Client,
    port: u16,
    token: &str,
    books_dir: &Path,
    source: &Path,
    docling_dir: Option<String>,
    layout: bool,
    fallback: &FallbackSource,
    on_event: F,
) -> Result<ImportResult, String>
where
    F: FnMut(ImportEvent),
{
    import_book_with_control(
        pool,
        client,
        port,
        token,
        books_dir,
        source,
        docling_dir,
        layout,
        fallback,
        None,
        on_event,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn ingest_into<F>(
    pool: &SqlitePool,
    client: &reqwest::Client,
    port: u16,
    token: &str,
    book_dir: &Path,
    source: &Path,
    book_id: &str,
    content_hash: &str,
    docling_dir: Option<String>,
    docling_artifacts_sha: Option<String>,
    layout: bool,
    fallback: &FallbackSource,
    control: Option<&mut ControlReceiver>,
    on_event: &mut F,
) -> Result<ImportResult, String>
where
    F: FnMut(ImportEvent),
{
    let extension = source_extension(source);
    tokio::fs::create_dir_all(book_dir)
        .await
        .map_err(|e| e.to_string())?;
    let dest: PathBuf = book_dir.join(format!("source.{extension}"));
    tokio::fs::copy(source, &dest)
        .await
        .map_err(|e| format!("cannot copy '{}': {e}", source.display()))?;

    let request = IngestRequest::build(
        pool,
        dest.to_string_lossy().into_owned(),
        book_id.to_string(),
        docling_dir,
        docling_artifacts_sha,
        layout,
        fallback,
    )
    .await;
    let mut outcome: Option<Outcome> = None;
    let mut page_rows: Vec<serde_json::Value> = Vec::new();
    let handle_event = |event: SseEvent| match event.event.as_str() {
        "progress" => {
            let page = event.data["page"].as_i64().unwrap_or(0);
            let total = event.data["total"].as_i64().unwrap_or(0);
            let stage = event.data["stage"]
                .as_str()
                .unwrap_or("extract")
                .to_string();
            on_event(ImportEvent::Progress { page, total, stage });
        }
        "page_blocks" => {
            if let Some(rows) = event.data["pages"].as_array() {
                page_rows.extend(rows.iter().cloned());
            }
        }
        "manifest" => outcome = Some(Outcome::Manifest(event.data.clone())),
        "cancelled" => outcome = Some(Outcome::Cancelled),
        "error" => {
            outcome = Some(Outcome::Error {
                code: event.data["code"].as_str().unwrap_or("ERR_INTERNAL").into(),
                message: event.data["message"].as_str().unwrap_or("").into(),
            });
        }
        _ => {}
    };

    match control {
        Some(ctrl) => {
            let target = SidecarTarget::new(client, port, token);
            let stream_outcome =
                post_sse_cancellable(target, "/ingest", book_id, request, ctrl, handle_event)
                    .await
                    .map_err(|error| error.to_string())?;
            if let StreamOutcome::Interrupted(_) = stream_outcome {
                return Err("import cancelled".into());
            }
        }
        None => {
            post_sse(
                client,
                port,
                token,
                "/ingest",
                book_id,
                &request,
                handle_event,
            )
            .await
            .map_err(|error| error.to_string())?;
        }
    }

    let manifest = match outcome {
        Some(Outcome::Manifest(manifest)) => manifest,
        Some(Outcome::Cancelled) => return Err("import cancelled".into()),
        Some(Outcome::Error { code, message }) => return Err(format!("{code}: {message}")),
        None => return Err("sidecar stream ended without a manifest".into()),
    };

    let page_count = manifest["page_count"].as_i64().unwrap_or(0);
    let blocks_version = manifest_str(&manifest, "blocks_version");
    let blocks_page_count = manifest["blocks_page_count"].as_i64();
    let record = book::Book {
        id: book_id.to_string(),
        title: manifest_meta(&manifest, "title").unwrap_or_else(|| title_fallback(source)),
        author: manifest_meta(&manifest, "author"),
        source_path: dest.to_string_lossy().into_owned(),
        format: extension,
        page_count: Some(page_count),
        language: manifest_str(&manifest, "language"),
        is_scanned: manifest["is_scanned"].as_bool().unwrap_or(false),
        ingest_path: manifest_str(&manifest, "path"),
        cover_path: None,
        content_hash: content_hash.to_string(),
        blocks_version,
        blocks_page_count,
        layout_analysed: layout_pass_ran(&manifest),
        reflowed: manifest["reflowed"].as_bool().unwrap_or(false),
        extracting_since: None,
        created_at: now_ms(),
    };
    let chapters: Vec<book::Chapter> = manifest["chapters"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|chapter| book::Chapter {
            id: new_id(),
            book_id: book_id.to_string(),
            idx: chapter["idx"].as_i64().unwrap_or(0),
            title: chapter["title"].as_str().unwrap_or("").to_string(),
            page_start: chapter["page_start"].as_i64().unwrap_or(1),
            page_end: chapter["page_end"].as_i64().unwrap_or(1),
            is_pseudo: chapter["is_pseudo"].as_bool().unwrap_or(false),
            word_count: chapter["word_count"].as_i64(),
        })
        .collect();
    let pages = build_pages(book_id, page_count, &page_rows);
    let with_blocks = pages.iter().filter(|p| p.text_json.is_some()).count();
    let flagged = pages.iter().filter(|p| p.needs_upgrade).count();
    if with_blocks == 0 {
        tracing::warn!(
            "import stored 0 of {page_count} page(s) with text blocks; the first \
             job on this book will re-extract it"
        );
    } else {
        tracing::info!(
            "import stored {with_blocks} of {page_count} page(s) with text blocks, \
             {flagged} flagged for upgrade"
        );
    }
    book::insert_book_with_contents(pool, &record, &chapters, &pages)
        .await
        .map_err(|e| e.to_string())?;

    on_event(ImportEvent::Done {
        book_id: book_id.to_string(),
    });
    let warnings: Vec<String> = manifest["warnings"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|w| w.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let book = summarize(pool, &record, None).await?;
    Ok(ImportResult { book, warnings })
}

pub async fn attach_cover(
    pool: &SqlitePool,
    cache_dir: &Path,
    books_dir: &Path,
    book_id: &str,
    page_hash: &str,
) -> Result<String, String> {
    let cached = crate::cache_path::cache_path(cache_dir, page_hash);
    let cover = books_dir.join(book_id).join("cover.png");
    tokio::fs::copy(&cached, &cover)
        .await
        .map_err(|e| format!("cannot copy cover from cache: {e}"))?;
    let cover_str = cover.to_string_lossy().into_owned();
    book::set_book_cover(pool, book_id, &cover_str)
        .await
        .map_err(|e| e.to_string())?;
    Ok(cover_str)
}

/// A backfill either finished, or the job that owns it was paused or cancelled.
///
/// Flattening an interruption into an error string made the worker treat a pause as a
/// non-retryable stage failure and drive the job to FAILED. Pause and cancel are outcomes,
/// not errors, and every other stage already returns them as such.
#[derive(Debug, Clone, PartialEq)]
pub enum BackfillResult {
    Completed(BackfillOutcome),
    Interrupted(StageOutcome),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BackfillOutcome {
    pub pages: usize,
    pub blocks_version: String,
    pub warnings: Vec<String>,
    /// Whether the layout model read the whole book, straight from the engine rather than
    /// inferred by the caller.
    pub layout_pass_ran: bool,
}

/// Did the layout model actually read this book?
///
/// The engine says so directly. Inferring it from the absence of degradation warnings
/// marked a scanned PDF and a spine EPUB as analysed even though the pass never ran for
/// either, which retired the retry behind `ERR_ALREADY_ANALYSED` for exactly the books
/// that still needed it.
pub fn layout_pass_ran(manifest: &serde_json::Value) -> bool {
    manifest["layout_pass_ran"].as_bool().unwrap_or(false)
}

fn chapter_key(chapter: &book::Chapter) -> (i64, i64, i64) {
    (chapter.idx, chapter.page_start, chapter.page_end)
}

#[allow(clippy::too_many_arguments)]
pub async fn backfill_blocks<F>(
    pool: &SqlitePool,
    target: SidecarTarget<'_>,
    book_row: &book::Book,
    docling_dir: Option<String>,
    intent: BackfillIntent<'_>,
    fallback: &FallbackSource,
    control: &mut ControlReceiver,
    mut on_progress: F,
) -> Result<BackfillResult, String>
where
    F: FnMut(i64, i64),
{
    let request = IngestRequest::build(
        pool,
        book_row.source_path.clone(),
        book_row.id.clone(),
        docling_dir,
        docling_artifacts_sha(pool).await,
        intent.layout,
        fallback,
    )
    .await;
    let mut outcome: Option<Outcome> = None;
    let mut page_rows: Vec<serde_json::Value> = Vec::new();
    let stream = post_sse_cancellable(
        target,
        "/ingest",
        &book_row.id,
        request,
        control,
        |event: SseEvent| match event.event.as_str() {
            "progress" => on_progress(
                event.data["page"].as_i64().unwrap_or(0),
                event.data["total"].as_i64().unwrap_or(0),
            ),
            "page_blocks" => {
                if let Some(rows) = event.data["pages"].as_array() {
                    page_rows.extend(rows.iter().cloned());
                }
            }
            "manifest" => outcome = Some(Outcome::Manifest(event.data.clone())),
            "cancelled" => outcome = Some(Outcome::Cancelled),
            "error" => {
                outcome = Some(Outcome::Error {
                    code: event.data["code"].as_str().unwrap_or("ERR_INTERNAL").into(),
                    message: event.data["message"].as_str().unwrap_or("").into(),
                });
            }
            _ => {}
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    if let StreamOutcome::Interrupted(interruption) = stream {
        return Ok(BackfillResult::Interrupted(interruption));
    }

    let manifest = match outcome {
        Some(Outcome::Manifest(manifest)) => manifest,
        Some(Outcome::Cancelled) => {
            return Ok(BackfillResult::Interrupted(StageOutcome::Cancelled))
        }
        Some(Outcome::Error { code, message }) => return Err(format!("{code}: {message}")),
        None => return Err("sidecar stream ended without a manifest".into()),
    };

    let page_count = manifest["page_count"].as_i64().unwrap_or(0);
    let warnings: Vec<String> = manifest["warnings"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let pass_ran = layout_pass_ran(&manifest);
    let blocks_version = manifest_str(&manifest, "blocks_version")
        .ok_or_else(|| "sidecar returned no blocks_version".to_string())?;
    let pages = build_pages(&book_row.id, page_count, &page_rows);
    let flagged = pages.iter().filter(|page| page.needs_upgrade).count();
    if pages.iter().all(|page| page.text_json.is_none()) {
        return Err("ERR_NO_BLOCKS: re-extraction produced no text".into());
    }

    let existing = book::list_chapters(pool, &book_row.id)
        .await
        .map_err(|e| e.to_string())?;
    let fresh = manifest_chapters(&book_row.id, &manifest);
    if fresh.is_empty() && !existing.is_empty() {
        return Err(
            "ERR_NO_CHAPTERS: re-extraction returned no chapters, so the book kept \
                    the ones it had"
                .into(),
        );
    }
    let remapped = match intent.alignment {
        ChapterAlignment::Strict => align_chapters(&existing, &fresh).inspect_err(|reason| {
            tracing::warn!("{reason}");
        })?,
        ChapterAlignment::Adopt => {
            let dependents = book::count_jobs_for_book(pool, &book_row.id)
                .await
                .map_err(|e| e.to_string())?;
            if dependents > 0 {
                tracing::info!(
                    "'{}' has {dependents} saved job(s), so its chapters are pinned and \
                     only the text improves",
                    book_row.title
                );
                pin_chapters(&existing, page_count)
            } else {
                adopt_chapters(&existing, &fresh)
            }
        }
    };

    book::replace_blocks(
        pool,
        &book_row.id,
        &pages,
        &remapped,
        book::Generation {
            blocks_version: &blocks_version,
            language: manifest_str(&manifest, "language").as_deref(),
            layout_analysed: intent.record_layout.then_some(pass_ran),
            owner: intent.owner,
            now: now_ms(),
        },
    )
    .await
    .map_err(unwrap_protocol_error)?;
    tracing::info!(
        "re-extraction stored {} page(s), {flagged} flagged for upgrade, \
         version={blocks_version}",
        pages.len()
    );

    Ok(BackfillResult::Completed(BackfillOutcome {
        pages: pages.len(),
        blocks_version,
        warnings,
        layout_pass_ran: pass_ran,
    }))
}

fn manifest_chapters(book_id: &str, manifest: &serde_json::Value) -> Vec<book::Chapter> {
    manifest["chapters"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|chapter| book::Chapter {
            id: String::new(),
            book_id: book_id.to_string(),
            idx: chapter["idx"].as_i64().unwrap_or(0),
            title: chapter["title"].as_str().unwrap_or("").to_string(),
            page_start: chapter["page_start"].as_i64().unwrap_or(1),
            page_end: chapter["page_end"].as_i64().unwrap_or(1),
            is_pseudo: chapter["is_pseudo"].as_bool().unwrap_or(false),
            word_count: chapter["word_count"].as_i64(),
        })
        .collect()
}

/// How much chapter movement a re-extraction is allowed to absorb.
///
/// Chapter detection is layout-dependent — `chapters_from_toc` picks its depth using
/// `page_words`, which shrinks once running heads stop counting — so a correct pass on a
/// TOC book can legitimately return different bounds. Failing on that would spend hours and
/// then tell the user to delete the book.
///
/// But chapter *identity* is load bearing. Ids are reused positionally, so inserting or
/// removing a chapter anywhere except the tail shifts every later position: a saved
/// `scope_json` and the `chapter_id` on already-rendered segments would silently resolve to
/// different content, which is exactly what `Strict` exists to refuse. Deleting a surplus
/// chapter is worse still — `script_segment.chapter_id` cascades, so finished audio simply
/// disappears while its job still reads DONE.
///
/// So the question is not "did the count change" but "can anything observe the change".
/// `Adopt` therefore adopts the new reading only while **no render job exists for the
/// book** — no segments to strand, no scope to repoint. Once a book has been rendered its
/// chapters are pinned and only the blocks improve, which is the part the user actually
/// asked for; the alternative is to spend hours and refuse, or to silently rewrite what
/// they already made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChapterAlignment {
    Strict,
    Adopt,
}

/// What a re-extraction is for, stated rather than inferred.
///
/// `layout` used to be read off the book row, so the caller had to fabricate a row with the
/// flag flipped to ask for a pass. The row is what the book *is*; this is what the caller
/// *wants*, and the two are only the same by coincidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackfillIntent<'a> {
    pub layout: bool,
    pub alignment: ChapterAlignment,
    /// Whether success should update `book.layout_analysed`. The job's own backfill is not
    /// the user electing a pass, so it leaves the book's provenance alone.
    pub record_layout: bool,
    /// The lease this pass holds, when it holds one. The publish is fenced on it so a pass
    /// whose lease aged out cannot overwrite the generation that replaced it.
    pub owner: Option<&'a str>,
}

impl<'a> BackfillIntent<'a> {
    pub fn for_job(layout: bool) -> Self {
        Self {
            layout,
            alignment: ChapterAlignment::Strict,
            record_layout: false,
            owner: None,
        }
    }

    pub fn for_user_request(owner: &'a str, layout: bool) -> Self {
        Self {
            layout,
            alignment: ChapterAlignment::Adopt,
            record_layout: layout,
            owner: Some(owner),
        }
    }
}

pub(crate) fn adopt_chapters(
    existing: &[book::Chapter],
    fresh: &[book::Chapter],
) -> Vec<book::Chapter> {
    fresh
        .iter()
        .enumerate()
        .map(|(index, new)| book::Chapter {
            id: existing
                .get(index)
                .map(|old| old.id.clone())
                .unwrap_or_else(new_id),
            ..new.clone()
        })
        .collect()
}

/// Keep the chapters a rendered book already has, clamped into the pages that survive.
///
/// Nothing is renumbered and nothing is deleted, so every segment keeps the parent it was
/// rendered against. The only edit is a bound that would otherwise point past the end of
/// the book after a re-extraction returned fewer pages.
///
/// Two accepted consequences when a re-extraction shrinks the page count, both preferred to
/// touching chapter identity. Chapters that fell entirely past the new end collapse onto the
/// last page rather than disappearing, so a rendered segment keeps a parent that resolves.
/// And `word_count` is left as it was, because it describes the text those chapters were
/// rendered from; recomputing it against the new blocks would make a finished job's stored
/// duration disagree with its own chapter.
pub fn pin_chapters(existing: &[book::Chapter], page_count: i64) -> Vec<book::Chapter> {
    existing
        .iter()
        .map(|chapter| book::Chapter {
            page_start: chapter.page_start.min(page_count.max(1)),
            page_end: chapter.page_end.min(page_count.max(1)),
            ..chapter.clone()
        })
        .collect()
}

fn align_chapters(
    existing: &[book::Chapter],
    fresh: &[book::Chapter],
) -> Result<Vec<book::Chapter>, String> {
    if existing.len() != fresh.len() {
        return Err(format!(
            "ERR_CHAPTERS_MOVED: re-extraction found {} chapters where the library has {}; \
             delete and re-import this book so saved jobs are not silently repointed",
            fresh.len(),
            existing.len()
        ));
    }
    let mut aligned = Vec::with_capacity(fresh.len());
    for (old, new) in existing.iter().zip(fresh.iter()) {
        if chapter_key(old) != chapter_key(new) {
            return Err(format!(
                "ERR_CHAPTERS_MOVED: chapter {} moved from pages {}-{} to {}-{}; delete and \
                 re-import this book so saved jobs are not silently repointed",
                old.idx, old.page_start, old.page_end, new.page_start, new.page_end
            ));
        }
        aligned.push(book::Chapter {
            id: old.id.clone(),
            ..new.clone()
        });
    }
    Ok(aligned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(idx: i64, start: i64, end: i64, title: &str) -> book::Chapter {
        book::Chapter {
            id: format!("chap-{idx}"),
            book_id: "book".into(),
            idx,
            title: title.into(),
            page_start: start,
            page_end: end,
            is_pseudo: false,
            word_count: Some(100),
        }
    }

    #[test]
    fn aligned_chapters_keep_their_ids_so_finished_jobs_survive() {
        let existing = vec![chapter(0, 1, 5, "One"), chapter(1, 6, 10, "Two")];
        let fresh = vec![chapter(0, 1, 5, "One (retitled)"), chapter(1, 6, 10, "Two")];

        let aligned = align_chapters(&existing, &fresh).expect("aligned");

        assert_eq!(aligned[0].id, "chap-0");
        assert_eq!(aligned[1].id, "chap-1");
        assert_eq!(
            aligned[0].title, "One (retitled)",
            "content refreshes, identity does not"
        );
    }

    #[test]
    fn a_moved_chapter_refuses_rather_than_repointing_a_saved_job() {
        let existing = vec![chapter(0, 1, 5, "One"), chapter(1, 6, 10, "Two")];
        let fresh = vec![chapter(0, 1, 4, "One"), chapter(1, 5, 10, "Two")];

        let err = align_chapters(&existing, &fresh).expect_err("must refuse");

        assert!(err.contains("ERR_CHAPTERS_MOVED"), "{err}");
    }

    #[test]
    fn a_different_chapter_count_refuses() {
        let existing = vec![chapter(0, 1, 10, "One")];
        let fresh = vec![chapter(0, 1, 5, "One"), chapter(1, 6, 10, "Two")];

        assert!(align_chapters(&existing, &fresh)
            .expect_err("must refuse")
            .contains("ERR_CHAPTERS_MOVED"));
    }

    #[tokio::test]
    async fn byok_readings_are_kept_in_the_book_directory_not_the_engine_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::storage::open_pool(&dir.path().join("db.sqlite"))
            .await
            .expect("open pool");
        crate::storage::settings::set_setting(
            &pool,
            crate::ingest_fallback::FALLBACK_ENABLED_KEY,
            "true",
        )
        .await
        .expect("set");
        let keys = std::sync::Arc::new(crate::voice::keys::KeyManager::session_only_for_tests());
        keys.add_key(
            &pool,
            "gemini",
            None,
            "AIzaSy-test-material-0123456789abcdef".to_string(),
            None,
        )
        .await
        .expect("add key");
        let book_dir = dir.path().join("books").join("book-1");
        let source = book_dir.join("source.pdf");
        let request = IngestRequest::build(
            &pool,
            source.to_string_lossy().into_owned(),
            "book-1".into(),
            None,
            None,
            false,
            &FallbackSource::from_manager(keys),
        )
        .await;
        let json = serde_json::to_value(&request).expect("json");
        assert_eq!(
            json["fallback"]["cache_dir"],
            book_dir
                .join("fallback-readings")
                .to_string_lossy()
                .as_ref()
        );
    }

    #[test]
    fn extension_lowercased_with_pdf_fallback() {
        assert_eq!(source_extension(Path::new("/x/Book.PDF")), "pdf");
        assert_eq!(source_extension(Path::new("/x/book.epub")), "epub");
        assert_eq!(source_extension(Path::new("/x/noext")), "pdf");
    }

    #[test]
    fn title_falls_back_to_file_stem() {
        assert_eq!(title_fallback(Path::new("/x/My Book.pdf")), "My Book");
    }

    #[test]
    fn placeholder_metadata_is_treated_as_absent() {
        let manifest = serde_json::json!({ "title": "Title", "author": " AUTHOR " });
        assert_eq!(manifest_meta(&manifest, "title"), None);
        assert_eq!(manifest_meta(&manifest, "author"), None);
        assert_eq!(
            manifest_meta(&manifest, "title")
                .unwrap_or_else(|| title_fallback(Path::new("/x/My Book.pdf"))),
            "My Book"
        );
    }

    #[test]
    fn real_metadata_survives_the_placeholder_filter() {
        let manifest = serde_json::json!({ "title": "A Title of Sorts", "author": "Ann Author" });
        assert_eq!(
            manifest_meta(&manifest, "title").as_deref(),
            Some("A Title of Sorts")
        );
        assert_eq!(
            manifest_meta(&manifest, "author").as_deref(),
            Some("Ann Author")
        );
    }

    #[test]
    fn docling_artifacts_dir_requires_existing_install() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(docling_artifacts_dir(tmp.path()), None);
        let dir = tmp.path().join("layout").join("docling-layout");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            docling_artifacts_dir(tmp.path()),
            Some(dir.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn manifest_str_trims_and_rejects_empty() {
        let manifest = serde_json::json!({"title": "  T  ", "author": "  ", "language": null});
        assert_eq!(manifest_str(&manifest, "title"), Some("T".into()));
        assert_eq!(manifest_str(&manifest, "author"), None);
        assert_eq!(manifest_str(&manifest, "language"), None);
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProbeResult {
    pub page_count: i64,
    pub reflowed: bool,
}

pub async fn probe_document(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    path: &str,
) -> Result<ProbeResult, String> {
    let response = client
        .post(format!("http://127.0.0.1:{port}/probe"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "book_path": path }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("probe failed: {}", response.status()));
    }
    response
        .json::<ProbeResult>()
        .await
        .map_err(|e| e.to_string())
}

/// Measured cost of the layout pass, per page, by device.
///
/// From `HARDWARE_PROFILER.md` §5: one workstation, docling-layout-heron, 25 pages after
/// a warm-up conversion. The 46x gap is the largest coefficient spread in the engine and
/// is the whole reason the import needs an estimate rather than a spinner — the same
/// textbook is six minutes on a mid-range GPU and over four hours on CPU.
pub const LAYOUT_SECONDS_PER_PAGE_GPU: f64 = 0.25;
pub const LAYOUT_SECONDS_PER_PAGE_CPU: f64 = 11.39;

/// Measured cost of the CPU pdfium extractor, per page.
///
/// From `docs/notes/soak-layout-validation.md` §4.0: 16,419 pages across the soak corpus
/// measured at 64.8 ms/page (0.065 s/page) overall.
pub const EXTRACT_SECONDS_PER_PAGE_CPU: f64 = 0.065;

/// Above this, the cost is worth a decision rather than a spinner.
pub const LAYOUT_PROMPT_SECONDS: f64 = 60.0;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlan {
    pub page_count: i64,
    pub reflowed: bool,
    pub layout_available: bool,
    pub layout_seconds: f64,
    pub on_gpu: bool,
    /// `true` when the estimate is small enough to just get on with it.
    pub silent: bool,
    /// What the dialog should start on. Declining is pre-selected only where the cost is
    /// punishing, which mirrors how the fit badges pre-select overnight for a heavy job.
    pub recommend_layout: bool,
    pub cpu_seconds: f64,
    pub offer_both: bool,
}

pub fn plan_layout(
    page_count: i64,
    reflowed: bool,
    layout_available: bool,
    on_gpu: bool,
) -> ImportPlan {
    let per_page = if on_gpu {
        LAYOUT_SECONDS_PER_PAGE_GPU
    } else {
        LAYOUT_SECONDS_PER_PAGE_CPU
    };
    let applicable = layout_available && !reflowed;
    let seconds = if applicable {
        page_count.max(0) as f64 * per_page
    } else {
        0.0
    };
    let cpu_seconds = page_count.max(0) as f64 * EXTRACT_SECONDS_PER_PAGE_CPU;
    let silent = !applicable || (on_gpu && applicable) || seconds < LAYOUT_PROMPT_SECONDS;
    let offer_both = applicable && !on_gpu;
    ImportPlan {
        page_count,
        reflowed,
        layout_available,
        layout_seconds: seconds,
        on_gpu,
        silent,
        recommend_layout: applicable && on_gpu,
        cpu_seconds,
        offer_both,
    }
}

/// Why a re-extraction cannot usefully run right now, or `None` when it can.
///
/// Permanent reasons are reported ahead of the transient ones: telling someone to wait for
/// a job to finish is wrong when the pass would never have helped this book anyway. This is
/// advisory, evaluated at plan time so the UI can explain itself before spending anything.
/// It is not the guard — `book::claim_extraction` is, because only one statement can decide
/// admission without a window between the test and the work.
///
/// "Already analysed" is a claim about a *generation*, not about the book: a book read by
/// an older extractor has plenty to gain, and refusing it is how a widened label harvest
/// would have reached every future import and no existing one.
pub fn extraction_blocked_reason(
    layout_analysed: bool,
    is_scanned: bool,
    plan: &ImportPlan,
    active_jobs: i64,
    extracting: bool,
    generation_current: bool,
) -> Option<String> {
    if layout_analysed && generation_current {
        return Some(
            "ERR_ALREADY_ANALYSED: this book was already analysed with the layout model".into(),
        );
    }
    if !plan.layout_available {
        return Some(
            "ERR_NO_LAYOUT_SUITE: the layout model is not installed, so re-extracting would \
             produce the same text"
                .into(),
        );
    }
    if plan.reflowed {
        return Some(
            "ERR_REFLOWED: this book has no fixed pages, so it has no running heads to find".into(),
        );
    }
    if is_scanned {
        return Some(
            "ERR_SCANNED: this book is scanned images, so re-reading would repeat the OCR and \
             produce the same text"
                .into(),
        );
    }
    if extracting {
        return Some(
            "ERR_EXTRACTION_ACTIVE: this book's text is already being re-extracted".into(),
        );
    }
    if active_jobs > 0 {
        return Some(
            "ERR_JOB_ACTIVE: an audio job is queued or running for this book; re-extracting now \
             would rewrite the text it reads. Let it finish, or cancel it"
                .into(),
        );
    }
    None
}

fn unwrap_protocol_error(error: sqlx::Error) -> String {
    match error {
        sqlx::Error::Protocol(message) => message,
        other => other.to_string(),
    }
}

fn refusal_message(refusal: book::ClaimRefusal) -> String {
    match refusal {
        book::ClaimRefusal::NoSuchBook => "ERR_NO_BOOK: this book is no longer in the library",
        book::ClaimRefusal::AlreadyExtracting => {
            "ERR_EXTRACTION_ACTIVE: this book's text is already being re-extracted"
        }
        book::ClaimRefusal::JobActive => {
            "ERR_JOB_ACTIVE: an audio job is queued or running for this book; re-extracting now \
             would rewrite the text it reads. Let it finish, or cancel it"
        }
    }
    .to_string()
}

/// Holds the book's claim alive, and gives it up however the pass ends.
///
/// Two jobs. It refreshes `extracting_since` while the pass runs, so a lease held by a live
/// process never looks abandoned to anyone else and one that died ages out on its own — no
/// start-up sweep, which could not tell a crashed holder from a second instance's running
/// one. And it releases on drop, so an unwind or a dropped future cannot leave a book
/// un-extractable, un-deletable and un-jobbable.
///
/// The happy path releases explicitly and awaits it, because a spawned release races the
/// caller's own next read: the book would briefly still report itself busy after the pass
/// had finished.
pub struct ExtractionLease {
    pool: SqlitePool,
    book_id: String,
    owner: String,
    heartbeat: tauri::async_runtime::JoinHandle<()>,
    released: bool,
    lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ExtractionLease {
    /// Whether the lease was taken from under this pass while it ran.
    ///
    /// Distinguishes "another pass took over" from "the user pressed Stop": both arrive as
    /// an interruption, but only one of them is the user's own doing.
    pub fn was_lost(&self) -> bool {
        self.lost.load(std::sync::atomic::Ordering::Acquire)
    }

    pub async fn loss_reason(&self) -> Option<&'static str> {
        if !self.was_lost() {
            return None;
        }
        let holder = book::extraction_owner(&self.pool, &self.book_id)
            .await
            .unwrap_or(None);
        Some(match holder {
            Some(current) if current != self.owner => {
                "ERR_LEASE_LOST: another re-extraction took over this book, so this one stopped"
            }
            _ => "ERR_CLAIM_LOST: this re-extraction lost its claim on the book and stopped",
        })
    }
}

impl ExtractionLease {
    pub fn hold(
        pool: &SqlitePool,
        book_id: &str,
        owner: &str,
        on_lost: Option<crate::orchestrator::stage::ControlHandle>,
    ) -> Self {
        let beat_pool = pool.clone();
        let beat_book = book_id.to_string();
        let beat_owner = owner.to_string();
        let lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let beat_lost = std::sync::Arc::clone(&lost);
        let heartbeat = tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(book::LEASE_REFRESH_MS)).await;
                match book::refresh_extraction(&beat_pool, &beat_book, &beat_owner, now_ms()).await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(
                            "the extraction lease for '{beat_book}' was taken by \
                             someone else; stopping this pass"
                        );
                        beat_lost.store(true, std::sync::atomic::Ordering::Release);
                        if let Some(handle) = on_lost {
                            handle.cancel();
                        }
                        return;
                    }
                    Err(error) => {
                        tracing::warn!("extraction lease refresh failed: {error}");
                    }
                }
            }
        });
        Self {
            pool: pool.clone(),
            book_id: book_id.to_string(),
            owner: owner.to_string(),
            heartbeat,
            released: false,
            lost,
        }
    }

    pub async fn release(&mut self) {
        self.heartbeat.abort();
        self.released = true;
        if let Err(error) = book::release_extraction(&self.pool, &self.book_id, &self.owner).await {
            tracing::warn!(
                "could not release the extraction lease for '{}': {error}; it ages \
                 out on its own",
                self.book_id
            );
        }
    }
}

impl Drop for ExtractionLease {
    fn drop(&mut self) {
        self.heartbeat.abort();
        if self.released {
            return;
        }
        let pool = self.pool.clone();
        let book_id = self.book_id.clone();
        let owner = self.owner.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = book::release_extraction(&pool, &book_id, &owner).await {
                tracing::warn!(
                    "could not release the extraction lease for '{book_id}': \
                     {error}; it ages out on its own"
                );
            }
        });
    }
}

/// Everything a user-initiated pass needs that is not the book or the sidecar.
pub struct PassContext<'a> {
    pub docling_dir: Option<String>,
    pub layout: bool,
    /// Identifies this pass so a release cannot free a successor's lease.
    pub owner: &'a str,
    pub control: &'a mut ControlReceiver,
    /// Stops the pass if the lease is lost mid-run, so it never reaches the publish.
    pub lose_handle: Option<crate::orchestrator::stage::ControlHandle>,
    pub fallback: FallbackSource,
}

pub async fn improve_extraction<C, F>(
    pool: &SqlitePool,
    target: SidecarTarget<'_>,
    book_row: &book::Book,
    pass: PassContext<'_>,
    on_claimed: C,
    on_progress: F,
) -> Result<BackfillOutcome, String>
where
    C: FnOnce(),
    F: FnMut(i64, i64),
{
    let PassContext {
        docling_dir,
        layout,
        owner,
        control,
        lose_handle,
        fallback,
    } = pass;
    if let Err(refusal) = book::claim_extraction(pool, &book_row.id, owner, now_ms())
        .await
        .map_err(|e| e.to_string())?
    {
        return Err(refusal_message(refusal));
    }
    on_claimed();
    let mut lease = ExtractionLease::hold(pool, &book_row.id, owner, lose_handle);

    let outcome = backfill_blocks(
        pool,
        target,
        book_row,
        if layout { docling_dir } else { None },
        BackfillIntent::for_user_request(owner, layout),
        &fallback,
        control,
        on_progress,
    )
    .await;

    let loss = lease.loss_reason().await;
    lease.release().await;

    match outcome? {
        BackfillResult::Completed(done) => Ok(done),
        BackfillResult::Interrupted(_) if loss.is_some() => Err(loss.unwrap_or_default().into()),
        BackfillResult::Interrupted(_) => Err("ERR_CANCELLED: re-extraction stopped".into()),
    }
}

#[cfg(test)]
mod layout_plan_tests {
    use super::*;

    #[test]
    fn case_1_without_the_suite_cpu_and_gpu_are_silent_with_no_offer() {
        let cpu_plan = plan_layout(613, false, false, false);
        assert!(cpu_plan.silent);
        assert!(!cpu_plan.recommend_layout);
        assert!(!cpu_plan.offer_both);
        assert_eq!(cpu_plan.layout_seconds, 0.0);
        assert!((cpu_plan.cpu_seconds - 613.0 * EXTRACT_SECONDS_PER_PAGE_CPU).abs() < 0.01);

        let gpu_plan = plan_layout(613, false, false, true);
        assert!(gpu_plan.silent);
        assert!(!gpu_plan.recommend_layout);
        assert!(!gpu_plan.offer_both);
        assert_eq!(gpu_plan.layout_seconds, 0.0);
    }

    #[test]
    fn case_2_cpu_only_machine_over_threshold_shows_tradeoff_dialog() {
        let plan = plan_layout(613, false, true, false);
        assert!(!plan.silent);
        assert!(!plan.recommend_layout);
        assert!(plan.offer_both);
        assert!(plan.layout_seconds > 6_000.0);
        assert!((plan.cpu_seconds - 613.0 * EXTRACT_SECONDS_PER_PAGE_CPU).abs() < 0.01);
    }

    #[test]
    fn case_2_cpu_threshold_edges_around_sixty_seconds() {
        let under = plan_layout(5, false, true, false);
        assert!(under.silent);
        assert!(!under.recommend_layout);
        assert!(under.offer_both);
        assert!(under.layout_seconds < LAYOUT_PROMPT_SECONDS);

        let over = plan_layout(6, false, true, false);
        assert!(!over.silent);
        assert!(!over.recommend_layout);
        assert!(over.offer_both);
        assert!(over.layout_seconds >= LAYOUT_PROMPT_SECONDS);
    }

    #[test]
    fn case_3_gpu_available_runs_directly_and_silently() {
        let short = plan_layout(120, false, true, true);
        assert!(short.silent);
        assert!(short.recommend_layout);
        assert!(!short.offer_both);

        let long = plan_layout(613, false, true, true);
        assert!(long.silent);
        assert!(long.recommend_layout);
        assert!(!long.offer_both);
        assert!((long.layout_seconds - 153.25).abs() < 0.01);

        let massive = plan_layout(2000, false, true, true);
        assert!(massive.silent);
        assert!(massive.recommend_layout);
        assert!(!massive.offer_both);
    }

    #[test]
    fn reflowed_epub_never_asks_because_the_pass_never_runs() {
        let cpu_plan = plan_layout(547, true, true, false);
        assert!(cpu_plan.silent);
        assert_eq!(cpu_plan.layout_seconds, 0.0);
        assert!(!cpu_plan.offer_both);

        let gpu_plan = plan_layout(547, true, true, true);
        assert!(gpu_plan.silent);
        assert_eq!(gpu_plan.layout_seconds, 0.0);
        assert!(!gpu_plan.offer_both);
    }

    #[test]
    fn non_positive_page_counts_default_to_zero_seconds_silently() {
        let zero = plan_layout(0, false, true, false);
        assert!(zero.silent);
        assert_eq!(zero.layout_seconds, 0.0);
        assert_eq!(zero.cpu_seconds, 0.0);

        let negative = plan_layout(-10, false, true, false);
        assert!(negative.silent);
        assert_eq!(negative.layout_seconds, 0.0);
        assert_eq!(negative.cpu_seconds, 0.0);
    }
}

#[cfg(test)]
mod chapter_alignment_tests {
    use super::*;

    fn chapter(id: &str, idx: i64, start: i64, end: i64) -> book::Chapter {
        book::Chapter {
            id: id.into(),
            book_id: "b".into(),
            idx,
            title: format!("C{idx}"),
            page_start: start,
            page_end: end,
            is_pseudo: false,
            word_count: Some(100),
        }
    }

    #[test]
    fn adopting_reuses_ids_positionally_which_is_why_it_is_gated() {
        let existing = vec![chapter("a", 0, 1, 10), chapter("b", 1, 11, 20)];
        let fresh = vec![
            chapter("x", 0, 1, 5),
            chapter("y", 1, 6, 12),
            chapter("z", 2, 13, 20),
        ];
        let adopted = adopt_chapters(&existing, &fresh);

        assert_eq!(adopted[0].id, "a");
        assert_eq!(adopted[1].id, "b");
        assert_ne!(adopted[1].page_start, existing[1].page_start);
        assert_eq!(
            adopted.len(),
            3,
            "id 'b' now spans different pages than the chapter a saved scope named, which \
             is exactly why adoption is refused once any job exists"
        );
    }

    #[test]
    fn pinning_keeps_identity_and_order_and_only_clamps_bounds() {
        let existing = vec![chapter("a", 0, 1, 10), chapter("b", 1, 11, 20)];
        let pinned = pin_chapters(&existing, 12);

        assert_eq!(
            pinned.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(
            pinned.iter().map(|c| c.idx).collect::<Vec<_>>(),
            vec![0, 1],
            "a rendered book's segments resolve through these; nothing may shift"
        );
        assert_eq!(pinned[0].page_end, 10);
        assert_eq!(
            pinned[1].page_end, 12,
            "clamped into the pages that survive"
        );
    }
}

#[cfg(test)]
mod extraction_guard_tests {
    use super::*;

    fn improvable() -> ImportPlan {
        plan_layout(613, false, true, true)
    }

    #[test]
    fn a_declined_book_with_the_suite_and_no_job_can_run() {
        assert_eq!(
            extraction_blocked_reason(false, false, &improvable(), 0, false, true),
            None
        );
    }

    #[test]
    fn a_book_already_analysed_has_nothing_to_gain() {
        let reason =
            extraction_blocked_reason(true, false, &improvable(), 0, false, true).expect("blocked");
        assert!(reason.starts_with("ERR_ALREADY_ANALYSED"), "{reason}");
    }

    #[test]
    fn without_the_suite_re_extracting_would_change_nothing() {
        let plan = plan_layout(613, false, false, true);
        let reason =
            extraction_blocked_reason(false, false, &plan, 0, false, true).expect("blocked");
        assert!(reason.starts_with("ERR_NO_LAYOUT_SUITE"), "{reason}");
    }

    #[test]
    fn a_reflowed_book_has_no_running_heads_to_find() {
        let plan = plan_layout(547, true, true, true);
        let reason =
            extraction_blocked_reason(false, false, &plan, 0, false, true).expect("blocked");
        assert!(reason.starts_with("ERR_REFLOWED"), "{reason}");
    }

    #[test]
    fn an_active_job_blocks_because_replace_blocks_would_race_it() {
        let reason = extraction_blocked_reason(false, false, &improvable(), 1, false, true)
            .expect("blocked");
        assert!(reason.starts_with("ERR_JOB_ACTIVE"), "{reason}");
    }

    #[test]
    fn a_scanned_book_is_refused_because_re_ocr_returns_the_same_text() {
        let reason =
            extraction_blocked_reason(false, true, &improvable(), 0, false, true).expect("blocked");
        assert!(reason.starts_with("ERR_SCANNED"), "{reason}");
    }

    #[test]
    fn a_pass_already_in_flight_is_refused() {
        let reason =
            extraction_blocked_reason(false, false, &improvable(), 0, true, true).expect("blocked");
        assert!(reason.starts_with("ERR_EXTRACTION_ACTIVE"), "{reason}");
    }

    #[test]
    fn a_book_analysed_by_an_older_extractor_is_offered_the_pass_again() {
        assert_eq!(
            extraction_blocked_reason(true, false, &improvable(), 0, false, false),
            None,
            "'already analysed' is a claim about a generation; a book read at extract=2 has \
             no code labels at all and every reason to be read again"
        );
    }

    #[test]
    fn a_stored_generation_matching_the_engine_is_current() {
        let expected = "v1|extract=3|roles=1|pymupdf=1.28.0|reflow=612x792@11";
        assert!(generation_is_current(
            Some(&format!("{expected}|docling=78b1cf11a2c38448")),
            Some(expected)
        ));
        assert!(
            generation_is_current(Some(expected), Some(expected)),
            "a stored version with no docling suffix is still the same generation"
        );
    }

    #[test]
    fn an_older_generation_or_a_missing_one_is_stale() {
        let expected = "v1|extract=3|roles=1|pymupdf=1.28.0|reflow=612x792@11";
        assert!(!generation_is_current(
            Some("v1|extract=2|roles=1|pymupdf=1.28.0|reflow=612x792@11|docling=78b1cf11a2c3"),
            Some(expected)
        ));
        assert!(!generation_is_current(None, Some(expected)));
    }

    #[test]
    fn the_pdfium_identity_is_a_generation_of_its_own() {
        let expected = "v1|extract=3|roles=1|pdfium=7947|pypdfium2=5.12.0|reflow=612x792@11";
        assert!(generation_is_current(
            Some(&format!("{expected}|docling=78b1cf11a2c38448")),
            Some(expected)
        ));
        assert!(generation_is_current(Some(expected), Some(expected)));
        assert!(!generation_is_current(
            Some("v1|extract=3|roles=1|pdfium=7900|pypdfium2=5.12.0|reflow=612x792@11"),
            Some(expected)
        ));
    }

    #[test]
    fn a_book_read_under_one_backend_identity_is_stale_under_the_other() {
        let native = "v1|extract=3|roles=1|pymupdf=1.28.0|reflow=612x792@11";
        let pdfium = "v1|extract=3|roles=1|pdfium=7947|pypdfium2=5.12.0|reflow=612x792@11";
        assert!(
            !generation_is_current(Some(native), Some(pdfium)),
            "blocks extracted by the native engine must be offered a re-read once the \
             engine reports the pdfium identity"
        );
        assert!(
            !generation_is_current(Some(pdfium), Some(native)),
            "and the flag going back to the native engine must not reuse pdfium blocks"
        );
        assert!(!generation_is_current(
            Some(&format!("{native}|docling=78b1cf11a2c38448")),
            Some(pdfium)
        ));
    }

    #[test]
    fn a_prefix_that_is_not_a_generation_boundary_is_stale() {
        assert!(
            !generation_is_current(
                Some("v1|extract=30|roles=1|pymupdf=1.28.0"),
                Some("v1|extract=3")
            ),
            "extract=30 starts with extract=3 as text and is a different generation"
        );
    }

    #[test]
    fn an_engine_that_cannot_answer_never_calls_a_book_stale() {
        assert!(
            generation_is_current(Some("v1|extract=1|roles=1"), None),
            "staleness costs the user a multi-minute pass, so it has to be proven"
        );
        assert!(generation_is_current(
            Some("v1|extract=1|roles=1"),
            Some("")
        ));
    }

    #[test]
    fn a_permanent_reason_outranks_the_transient_one() {
        let plan = plan_layout(613, false, false, true);
        let reason =
            extraction_blocked_reason(false, false, &plan, 3, false, true).expect("blocked");
        assert!(
            reason.starts_with("ERR_NO_LAYOUT_SUITE"),
            "telling someone to wait for a job is wrong when the pass would never help: {reason}"
        );
    }
}
