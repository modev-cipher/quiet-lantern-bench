use sqlx::SqlitePool;

#[derive(Debug, Clone, PartialEq)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    pub source_path: String,
    pub format: String,
    pub page_count: Option<i64>,
    pub language: Option<String>,
    pub is_scanned: bool,
    pub ingest_path: Option<String>,
    pub cover_path: Option<String>,
    pub content_hash: String,
    pub blocks_version: Option<String>,
    pub blocks_page_count: Option<i64>,
    pub layout_analysed: bool,
    pub reflowed: bool,
    pub extracting_since: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    pub id: String,
    pub book_id: String,
    pub idx: i64,
    pub title: String,
    pub page_start: i64,
    pub page_end: i64,
    pub is_pseudo: bool,
    pub word_count: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub id: String,
    pub book_id: String,
    pub page_no: i64,
    pub width_pt: Option<f64>,
    pub height_pt: Option<f64>,
    pub render_hash: Option<String>,
    pub text_json: Option<String>,
    pub needs_upgrade: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FigureRegion {
    pub id: String,
    pub book_id: String,
    pub page_no: i64,
    pub bbox_json: String,
    pub kind: String,
    pub confidence: f64,
    pub crop_hash: Option<String>,
    pub caption: Option<String>,
    pub explanation: Option<String>,
    pub caption_confidence: Option<f64>,
}

pub async fn insert_book(pool: &SqlitePool, book: &Book) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO book (id, title, author, source_path, format, page_count, language, is_scanned, ingest_path, cover_path, content_hash, blocks_version, blocks_page_count, layout_analysed, reflowed, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        book.id,
        book.title,
        book.author,
        book.source_path,
        book.format,
        book.page_count,
        book.language,
        book.is_scanned,
        book.ingest_path,
        book.cover_path,
        book.content_hash,
        book.blocks_version,
        book.blocks_page_count,
        book.layout_analysed,
        book.reflowed,
        book.created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_book(pool: &SqlitePool, id: &str) -> Result<Option<Book>, sqlx::Error> {
    sqlx::query_as!(
        Book,
        r#"SELECT id as "id!", title, author, source_path, format, page_count, language,
                  is_scanned as "is_scanned: bool", ingest_path, cover_path, content_hash,
                  blocks_version, blocks_page_count, layout_analysed as "layout_analysed!: bool",
                  reflowed as "reflowed!: bool", extracting_since, created_at
           FROM book WHERE id = ?"#,
        id
    )
    .fetch_optional(pool)
    .await
}

pub async fn insert_book_with_contents(
    pool: &SqlitePool,
    book: &Book,
    chapters: &[Chapter],
    pages: &[Page],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "INSERT INTO book (id, title, author, source_path, format, page_count, language, is_scanned, ingest_path, cover_path, content_hash, blocks_version, blocks_page_count, layout_analysed, reflowed, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        book.id,
        book.title,
        book.author,
        book.source_path,
        book.format,
        book.page_count,
        book.language,
        book.is_scanned,
        book.ingest_path,
        book.cover_path,
        book.content_hash,
        book.blocks_version,
        book.blocks_page_count,
        book.layout_analysed,
        book.reflowed,
        book.created_at,
    )
    .execute(&mut *tx)
    .await?;
    for chapter in chapters {
        sqlx::query!(
            "INSERT INTO chapter (id, book_id, idx, title, page_start, page_end, is_pseudo, word_count)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            chapter.id,
            chapter.book_id,
            chapter.idx,
            chapter.title,
            chapter.page_start,
            chapter.page_end,
            chapter.is_pseudo,
            chapter.word_count,
        )
        .execute(&mut *tx)
        .await?;
    }
    for page in pages {
        sqlx::query!(
            "INSERT INTO page (id, book_id, page_no, width_pt, height_pt, render_hash, text_json, needs_upgrade)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            page.id,
            page.book_id,
            page.page_no,
            page.width_pt,
            page.height_pt,
            page.render_hash,
            page.text_json,
            page.needs_upgrade,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn find_book_by_content_hash(
    pool: &SqlitePool,
    content_hash: &str,
) -> Result<Option<Book>, sqlx::Error> {
    sqlx::query_as!(
        Book,
        r#"SELECT id as "id!", title, author, source_path, format, page_count, language,
                  is_scanned as "is_scanned: bool", ingest_path, cover_path, content_hash,
                  blocks_version, blocks_page_count, layout_analysed as "layout_analysed!: bool",
                  reflowed as "reflowed!: bool", extracting_since, created_at
           FROM book WHERE content_hash = ?"#,
        content_hash
    )
    .fetch_optional(pool)
    .await
}

pub async fn set_book_cover(
    pool: &SqlitePool,
    id: &str,
    cover_path: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE book SET cover_path = ? WHERE id = ?",
        cover_path,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_layout_analysed(
    pool: &SqlitePool,
    id: &str,
    analysed: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE book SET layout_analysed = ? WHERE id = ?",
        analysed,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub const TERMINAL_JOB_STATES: [&str; 3] = ["DONE", "CANCELLED", "FAILED"];

pub const TERMINAL_STATES_SQL: &str = "('DONE', 'CANCELLED', 'FAILED')";

pub async fn count_jobs_for_book(pool: &SqlitePool, book_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) as "count!: i64" FROM render_job WHERE book_id = ?"#,
        book_id
    )
    .fetch_one(pool)
    .await
}

pub async fn count_active_jobs(pool: &SqlitePool, book_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) as "count!: i64" FROM render_job
           WHERE book_id = ? AND state NOT IN ('DONE', 'CANCELLED', 'FAILED')"#,
        book_id
    )
    .fetch_one(pool)
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimRefusal {
    NoSuchBook,
    AlreadyExtracting,
    JobActive,
}

pub const LEASE_STALE_MS: i64 = 60_000;
pub const LEASE_REFRESH_MS: u64 = 10_000;

pub async fn claim_extraction(
    pool: &SqlitePool,
    book_id: &str,
    owner: &str,
    now: i64,
) -> Result<Result<(), ClaimRefusal>, sqlx::Error> {
    let stale_before = now - LEASE_STALE_MS;
    let claimed = sqlx::query!(
        "UPDATE book SET extracting_since = ?, extracting_owner = ?
         WHERE id = ?
           AND (extracting_since IS NULL OR extracting_since < ?)
           AND NOT EXISTS (
             SELECT 1 FROM render_job
             WHERE render_job.book_id = book.id
               AND render_job.state NOT IN ('DONE', 'CANCELLED', 'FAILED')
           )",
        now,
        owner,
        book_id,
        stale_before
    )
    .execute(pool)
    .await?;
    if claimed.rows_affected() == 1 {
        return Ok(Ok(()));
    }

    let existing: Option<Option<i64>> =
        sqlx::query_scalar!("SELECT extracting_since FROM book WHERE id = ?", book_id)
            .fetch_optional(pool)
            .await?;
    Ok(Err(match existing {
        None => ClaimRefusal::NoSuchBook,
        Some(Some(_)) => ClaimRefusal::AlreadyExtracting,
        Some(None) => ClaimRefusal::JobActive,
    }))
}

pub async fn release_extraction(
    pool: &SqlitePool,
    book_id: &str,
    owner: &str,
) -> Result<bool, sqlx::Error> {
    let released = sqlx::query!(
        "UPDATE book SET extracting_since = NULL, extracting_owner = NULL
         WHERE id = ? AND extracting_owner = ?",
        book_id,
        owner
    )
    .execute(pool)
    .await?;
    Ok(released.rows_affected() == 1)
}

pub async fn refresh_extraction(
    pool: &SqlitePool,
    book_id: &str,
    owner: &str,
    now: i64,
) -> Result<bool, sqlx::Error> {
    let stale_before = now - LEASE_STALE_MS;
    let touched = sqlx::query!(
        "UPDATE book SET extracting_since = ?
         WHERE id = ? AND extracting_owner = ? AND extracting_since >= ?",
        now,
        book_id,
        owner,
        stale_before
    )
    .execute(pool)
    .await?;
    Ok(touched.rows_affected() == 1)
}

pub async fn extraction_owner(
    pool: &SqlitePool,
    book_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let owner: Option<Option<String>> =
        sqlx::query_scalar!("SELECT extracting_owner FROM book WHERE id = ?", book_id)
            .fetch_optional(pool)
            .await?;
    Ok(owner.flatten())
}

pub async fn is_extracting(
    pool: &SqlitePool,
    book_id: &str,
    now: i64,
) -> Result<bool, sqlx::Error> {
    let lease: Option<Option<i64>> =
        sqlx::query_scalar!("SELECT extracting_since FROM book WHERE id = ?", book_id)
            .fetch_optional(pool)
            .await?;
    Ok(lease
        .flatten()
        .is_some_and(|since| since >= now - LEASE_STALE_MS))
}

pub async fn set_reflowed(pool: &SqlitePool, id: &str, reflowed: bool) -> Result<(), sqlx::Error> {
    sqlx::query!("UPDATE book SET reflowed = ? WHERE id = ?", reflowed, id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn count_chapters(pool: &SqlitePool, book_id: &str) -> Result<i64, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) as "count!: i64" FROM chapter WHERE book_id = ?"#,
        book_id
    )
    .fetch_one(pool)
    .await?;
    Ok(row.count)
}

pub async fn list_books(pool: &SqlitePool) -> Result<Vec<Book>, sqlx::Error> {
    sqlx::query_as!(
        Book,
        r#"SELECT id as "id!", title, author, source_path, format, page_count, language,
                  is_scanned as "is_scanned: bool", ingest_path, cover_path, content_hash,
                  blocks_version, blocks_page_count, layout_analysed as "layout_analysed!: bool",
                  reflowed as "reflowed!: bool", extracting_since, created_at
           FROM book ORDER BY created_at DESC, id DESC"#
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteRefusal {
    NoSuchBook,
    ExtractionActive,
    JobActive,
}

pub async fn delete_book_unless_busy(
    pool: &SqlitePool,
    id: &str,
    now: i64,
) -> Result<Result<(), DeleteRefusal>, sqlx::Error> {
    let stale_before = now - LEASE_STALE_MS;
    let mut tx = pool.begin().await?;
    let deleted = sqlx::query(
        "DELETE FROM book WHERE id = ?
           AND (extracting_since IS NULL OR extracting_since < ?)
           AND NOT EXISTS (
             SELECT 1 FROM render_job
             WHERE render_job.book_id = book.id
               AND render_job.state NOT IN ('DONE', 'CANCELLED', 'FAILED')
           )",
    )
    .bind(id)
    .bind(stale_before)
    .execute(&mut *tx)
    .await?;
    if deleted.rows_affected() == 1 {
        sqlx::query(
            "INSERT INTO app_setting (key, value, updated_at) VALUES (?, '', ?)
             ON CONFLICT(key) DO NOTHING",
        )
        .bind(super::book_files::pending_removal_key(id))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Ok(()));
    }
    tx.rollback().await?;

    let existing: Option<Option<i64>> =
        sqlx::query_scalar("SELECT extracting_since FROM book WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(Err(match existing {
        None => DeleteRefusal::NoSuchBook,
        Some(Some(since)) if since >= stale_before => DeleteRefusal::ExtractionActive,
        Some(_) => DeleteRefusal::JobActive,
    }))
}

pub async fn delete_book(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM book WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn insert_chapter(pool: &SqlitePool, chapter: &Chapter) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO chapter (id, book_id, idx, title, page_start, page_end, is_pseudo, word_count)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        chapter.id,
        chapter.book_id,
        chapter.idx,
        chapter.title,
        chapter.page_start,
        chapter.page_end,
        chapter.is_pseudo,
        chapter.word_count,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_chapters(pool: &SqlitePool, book_id: &str) -> Result<Vec<Chapter>, sqlx::Error> {
    sqlx::query_as!(
        Chapter,
        r#"SELECT id as "id!", book_id, idx, title, page_start, page_end,
                  is_pseudo as "is_pseudo: bool", word_count
           FROM chapter WHERE book_id = ? ORDER BY idx"#,
        book_id
    )
    .fetch_all(pool)
    .await
}

pub async fn insert_page(pool: &SqlitePool, page: &Page) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO page (id, book_id, page_no, width_pt, height_pt, render_hash, text_json, needs_upgrade)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        page.id,
        page.book_id,
        page.page_no,
        page.width_pt,
        page.height_pt,
        page.render_hash,
        page.text_json,
        page.needs_upgrade,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_page(
    pool: &SqlitePool,
    book_id: &str,
    page_no: i64,
) -> Result<Option<Page>, sqlx::Error> {
    sqlx::query_as!(
        Page,
        r#"SELECT id as "id!", book_id, page_no, width_pt, height_pt, render_hash, text_json,
                  needs_upgrade as "needs_upgrade: bool"
           FROM page WHERE book_id = ? AND page_no = ?"#,
        book_id,
        page_no
    )
    .fetch_optional(pool)
    .await
}

pub async fn set_page_text(
    pool: &SqlitePool,
    page_id: &str,
    text_json: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE page SET text_json = ? WHERE id = ?",
        text_json,
        page_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_page_render_hash(
    pool: &SqlitePool,
    page_id: &str,
    render_hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE page SET render_hash = ? WHERE id = ?",
        render_hash,
        page_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageGeometry {
    pub page_id: String,
    pub geometry_version: String,
    pub geometry_json: String,
}

pub async fn upsert_page_geometry(
    pool: &SqlitePool,
    geometry: &PageGeometry,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO page_geometry (page_id, geometry_version, geometry_json)
         VALUES (?, ?, ?)
         ON CONFLICT(page_id) DO UPDATE SET
            geometry_version = excluded.geometry_version,
            geometry_json = excluded.geometry_json",
        geometry.page_id,
        geometry.geometry_version,
        geometry.geometry_json,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn upsert_page_geometry_fenced(
    pool: &SqlitePool,
    geometry: &PageGeometry,
    book_id: &str,
    blocks_version: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let affected = sqlx::query!(
        "INSERT INTO page_geometry (page_id, geometry_version, geometry_json)
         SELECT ?, ?, ?
         WHERE EXISTS (
            SELECT 1 FROM page p
            JOIN book b ON b.id = p.book_id
            WHERE p.id = ? AND b.id = ? AND b.blocks_version IS ?
         )
         ON CONFLICT(page_id) DO UPDATE SET
            geometry_version = excluded.geometry_version,
            geometry_json = excluded.geometry_json",
        geometry.page_id,
        geometry.geometry_version,
        geometry.geometry_json,
        geometry.page_id,
        book_id,
        blocks_version,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

pub async fn get_page_geometry(
    pool: &SqlitePool,
    page_id: &str,
) -> Result<Option<PageGeometry>, sqlx::Error> {
    sqlx::query_as!(
        PageGeometry,
        r#"SELECT page_id as "page_id!", geometry_version, geometry_json
           FROM page_geometry WHERE page_id = ?"#,
        page_id
    )
    .fetch_optional(pool)
    .await
}

pub async fn insert_figure_region(
    pool: &SqlitePool,
    figure: &FigureRegion,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO figure_region (id, book_id, page_no, bbox_json, type, confidence, crop_hash, caption, explanation, caption_confidence)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        figure.id,
        figure.book_id,
        figure.page_no,
        figure.bbox_json,
        figure.kind,
        figure.confidence,
        figure.crop_hash,
        figure.caption,
        figure.explanation,
        figure.caption_confidence,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn insert_figure_region_ignore_conflict(
    pool: &SqlitePool,
    figure: &FigureRegion,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO figure_region (id, book_id, page_no, bbox_json, type, confidence, crop_hash, caption, explanation, caption_confidence)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO NOTHING",
        figure.id,
        figure.book_id,
        figure.page_no,
        figure.bbox_json,
        figure.kind,
        figure.confidence,
        figure.crop_hash,
        figure.caption,
        figure.explanation,
        figure.caption_confidence,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_figure_region(
    pool: &SqlitePool,
    book_id: &str,
    figure_id: &str,
) -> Result<Option<FigureRegion>, sqlx::Error> {
    sqlx::query_as!(
        FigureRegion,
        r#"SELECT id as "id!", book_id, page_no, bbox_json, type as "kind!", confidence,
                  crop_hash, caption, explanation, caption_confidence
           FROM figure_region WHERE id = ? AND book_id = ?"#,
        figure_id,
        book_id
    )
    .fetch_optional(pool)
    .await
}

pub async fn list_figures_for_page(
    pool: &SqlitePool,
    book_id: &str,
    page_no: i64,
) -> Result<Vec<FigureRegion>, sqlx::Error> {
    sqlx::query_as!(
        FigureRegion,
        r#"SELECT id as "id!", book_id, page_no, bbox_json, type as "kind!", confidence,
                  crop_hash, caption, explanation, caption_confidence
           FROM figure_region WHERE book_id = ? AND page_no = ?"#,
        book_id,
        page_no
    )
    .fetch_all(pool)
    .await
}

pub async fn list_figures_for_book(
    pool: &SqlitePool,
    book_id: &str,
) -> Result<Vec<FigureRegion>, sqlx::Error> {
    sqlx::query_as!(
        FigureRegion,
        r#"SELECT id as "id!", book_id, page_no, bbox_json, type as "kind!", confidence,
                  crop_hash, caption, explanation, caption_confidence
           FROM figure_region WHERE book_id = ? ORDER BY page_no, id"#,
        book_id
    )
    .fetch_all(pool)
    .await
}

pub async fn set_figure_vision_output(
    pool: &SqlitePool,
    figure_id: &str,
    caption: &str,
    explanation: &str,
    caption_confidence: f64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE figure_region SET caption = ?, explanation = ?, caption_confidence = ? WHERE id = ?",
        caption,
        explanation,
        caption_confidence,
        figure_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_pages(pool: &SqlitePool, book_id: &str) -> Result<Vec<Page>, sqlx::Error> {
    sqlx::query_as!(
        Page,
        r#"SELECT id as "id!", book_id, page_no, width_pt, height_pt, render_hash, text_json,
                  needs_upgrade as "needs_upgrade: bool"
           FROM page WHERE book_id = ? ORDER BY page_no"#,
        book_id
    )
    .fetch_all(pool)
    .await
}

pub async fn update_chapter_bounds(
    pool: &SqlitePool,
    chapter: &Chapter,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE chapter SET title = ?, page_start = ?, page_end = ?, is_pseudo = ?,
                word_count = ? WHERE id = ?",
        chapter.title,
        chapter.page_start,
        chapter.page_end,
        chapter.is_pseudo,
        chapter.word_count,
        chapter.id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub struct Generation<'a> {
    pub blocks_version: &'a str,
    pub language: Option<&'a str>,
    pub layout_analysed: Option<bool>,
    pub owner: Option<&'a str>,
    pub now: i64,
}

pub async fn replace_blocks(
    pool: &SqlitePool,
    book_id: &str,
    pages: &[Page],
    chapters: &[Chapter],
    generation: Generation<'_>,
) -> Result<(), sqlx::Error> {
    let Generation {
        blocks_version,
        language,
        layout_analysed,
        owner,
        now,
    } = generation;
    let fence_stale_before = now - LEASE_STALE_MS;
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "DELETE FROM page_geometry
         WHERE page_id IN (SELECT id FROM page WHERE book_id = ?)",
        book_id
    )
    .execute(&mut *tx)
    .await?;
    for page in pages {
        sqlx::query!(
            "INSERT INTO page (id, book_id, page_no, width_pt, height_pt, render_hash,
                               text_json, needs_upgrade)
             VALUES (?, ?, ?, ?, ?, NULL, ?, ?)
             ON CONFLICT(book_id, page_no) DO UPDATE SET
                width_pt = excluded.width_pt,
                height_pt = excluded.height_pt,
                text_json = excluded.text_json,
                needs_upgrade = excluded.needs_upgrade,
                render_hash = NULL",
            page.id,
            page.book_id,
            page.page_no,
            page.width_pt,
            page.height_pt,
            page.text_json,
            page.needs_upgrade,
        )
        .execute(&mut *tx)
        .await?;
    }
    for chapter in chapters {
        let updated = sqlx::query!(
            "UPDATE chapter SET idx = ?, title = ?, page_start = ?, page_end = ?,
                    is_pseudo = ?, word_count = ? WHERE id = ? AND book_id = ?",
            chapter.idx,
            chapter.title,
            chapter.page_start,
            chapter.page_end,
            chapter.is_pseudo,
            chapter.word_count,
            chapter.id,
            book_id,
        )
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            sqlx::query!(
                "INSERT INTO chapter (id, book_id, idx, title, page_start, page_end,
                                      is_pseudo, word_count)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                chapter.id,
                book_id,
                chapter.idx,
                chapter.title,
                chapter.page_start,
                chapter.page_end,
                chapter.is_pseudo,
                chapter.word_count,
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    let keep: Vec<String> = chapters.iter().map(|chapter| chapter.id.clone()).collect();
    if !keep.is_empty() {
        let placeholders = vec!["?"; keep.len()].join(", ");
        let sql = format!("DELETE FROM chapter WHERE book_id = ? AND id NOT IN ({placeholders})");
        let mut delete = sqlx::query(&sql).bind(book_id);
        for id in &keep {
            delete = delete.bind(id);
        }
        let surplus = delete.execute(&mut *tx).await?;
        if surplus.rows_affected() > 0 {
            tracing::info!(
                "re-extraction dropped {} chapter(s) the new reading does not have",
                surplus.rows_affected()
            );
        }
    }
    let page_count = pages.len() as i64;
    sqlx::query!(
        "DELETE FROM page WHERE book_id = ? AND page_no > ?",
        book_id,
        page_count,
    )
    .execute(&mut *tx)
    .await?;
    let published = sqlx::query!(
        "UPDATE book SET blocks_version = ?, blocks_page_count = ?, page_count = ?,
                language = COALESCE(?, language),
                layout_analysed = COALESCE(?, layout_analysed)
         WHERE id = ?
           AND (
             ? IS NULL
             OR (extracting_owner = ? AND extracting_since >= ?)
           )",
        blocks_version,
        page_count,
        page_count,
        language,
        layout_analysed,
        book_id,
        owner,
        owner,
        fence_stale_before,
    )
    .execute(&mut *tx)
    .await?;
    if published.rows_affected() == 0 {
        return Err(sqlx::Error::Protocol(
            "ERR_LEASE_LOST: the extraction lease moved on before this pass could publish; \
             its work was discarded rather than overwriting a newer generation"
                .into(),
        ));
    }
    tx.commit().await
}
