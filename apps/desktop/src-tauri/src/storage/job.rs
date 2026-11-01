use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

use super::now_ms;
use crate::words::WordsJson;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RenderJob {
    pub id: String,
    pub book_id: String,
    pub scope_json: String,
    pub mode: String,
    pub voice_profile_id: String,
    pub output_format: String,
    pub model_overrides_json: Option<String>,
    pub resource_plan_json: Option<String>,
    pub vision_plan_json: Option<String>,
    pub tts_plan_json: Option<String>,
    pub align_plan_json: Option<String>,
    pub state: String,
    pub checkpoint_json: Option<String>,
    pub overnight: bool,
    pub eta_seconds: Option<i64>,
    pub report_json: Option<String>,
    pub output_paths_json: Option<String>,
    pub tts_seed: Option<i64>,
    pub verify: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

pub fn default_tts_seed(voice_profile_id: &str, content_hash: &str) -> i64 {
    let digest = Sha256::digest(format!("{voice_profile_id}:{content_hash}").as_bytes());
    let bytes: [u8; 4] = digest[..4].try_into().expect("sha256 yields 32 bytes");
    u32::from_be_bytes(bytes) as i64
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScriptSegment {
    pub id: String,
    pub job_id: String,
    pub chapter_id: String,
    pub idx: i64,
    pub page_no: i64,
    pub kind: String,
    pub figure_id: Option<String>,
    pub text: String,
    pub source_span_json: Option<String>,
    pub flags_json: Option<String>,
    pub audio_hash: Option<String>,
    pub duration_ms: Option<i64>,
    pub original_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    pub segment_id: String,
    pub source: String,
    pub words_json: WordsJson,
    pub wer: Option<f64>,
}

/// Seed a job row outside the extraction lease, for callers that own no book lifecycle.
///
/// It delegates rather than repeating the statement: a second column list is how `verify`
/// came to be bound here and dropped on the path `create_job` actually runs (#111).
pub async fn insert_job(pool: &SqlitePool, job: &RenderJob) -> Result<(), sqlx::Error> {
    if insert_job_unless_extracting(pool, job, now_ms()).await? {
        Ok(())
    } else {
        Err(sqlx::Error::RowNotFound)
    }
}

/// Insert only while a *live* re-extraction does not hold this book.
///
/// The mirror of `book::claim_extraction`: one statement, so the lease test happens under
/// the write lock that performs the insert. Whichever of the two commits first, the other
/// sees it and reports no rows, so the exclusion is symmetric rather than a read that the
/// other side can race.
///
/// It honours `LEASE_STALE_MS` for the same reason every other side of the lease does.
/// There is no start-up sweep any more, so a lease is given up by its holder or it ages
/// out; a bare `IS NOT NULL` here would let a crashed pass block job creation for this book
/// permanently, with no way back short of deleting and re-importing it.
pub async fn insert_job_unless_extracting(
    pool: &SqlitePool,
    job: &RenderJob,
    now: i64,
) -> Result<bool, sqlx::Error> {
    let stale_before = now - crate::storage::book::LEASE_STALE_MS;
    let inserted = sqlx::query!(
        "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, output_format,
                                 model_overrides_json, resource_plan_json, vision_plan_json,
                                 tts_plan_json, align_plan_json, state,
                                 checkpoint_json, overnight, eta_seconds, report_json,
                                 output_paths_json, tts_seed, verify, created_at, updated_at)
         SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
         WHERE NOT EXISTS (
           SELECT 1 FROM book
           WHERE book.id = ? AND book.extracting_since IS NOT NULL
             AND book.extracting_since >= ?
         )",
        job.id,
        job.book_id,
        job.scope_json,
        job.mode,
        job.voice_profile_id,
        job.output_format,
        job.model_overrides_json,
        job.resource_plan_json,
        job.vision_plan_json,
        job.tts_plan_json,
        job.align_plan_json,
        job.state,
        job.checkpoint_json,
        job.overnight,
        job.eta_seconds,
        job.report_json,
        job.output_paths_json,
        job.tts_seed,
        job.verify,
        job.created_at,
        job.updated_at,
        job.book_id,
        stale_before,
    )
    .execute(pool)
    .await?;
    Ok(inserted.rows_affected() == 1)
}

pub async fn get_job(pool: &SqlitePool, id: &str) -> Result<Option<RenderJob>, sqlx::Error> {
    sqlx::query_as!(
        RenderJob,
        r#"SELECT id as "id!", book_id, scope_json, mode, voice_profile_id, output_format,
                  model_overrides_json, resource_plan_json, vision_plan_json, tts_plan_json,
                  align_plan_json, state, checkpoint_json,
                  overnight as "overnight: bool", eta_seconds, report_json, output_paths_json,
                  tts_seed, verify as "verify: bool", created_at, updated_at
           FROM render_job WHERE id = ?"#,
        id
    )
    .fetch_optional(pool)
    .await
}

pub async fn list_jobs_in_states(
    pool: &SqlitePool,
    states: &[&str],
) -> Result<Vec<RenderJob>, sqlx::Error> {
    if states.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; states.len()].join(", ");
    let sql = format!(
        r#"SELECT id, book_id, scope_json, mode, voice_profile_id, output_format,
                  model_overrides_json, resource_plan_json, state, checkpoint_json,
                  overnight, eta_seconds, report_json, output_paths_json,
                  created_at, updated_at, vision_plan_json, tts_plan_json, align_plan_json,
                  tts_seed, verify
           FROM render_job WHERE state IN ({placeholders}) ORDER BY created_at, id"#
    );
    let mut query = sqlx::query_as::<_, RenderJob>(&sql);
    for state in states {
        query = query.bind(state);
    }
    query.fetch_all(pool).await
}

pub async fn count_jobs_for_voice_profile(
    pool: &SqlitePool,
    voice_profile_id: &str,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) as "count!: i64" FROM render_job WHERE voice_profile_id = ?"#,
        voice_profile_id
    )
    .fetch_one(pool)
    .await?;
    Ok(row.count)
}

pub async fn delete_job_if_settled(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM render_job WHERE id = ? AND state IN ('DONE', 'CANCELLED', 'FAILED', 'PAUSED')",
        id
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// C17: the cast the worker runs next — the oldest QUEUED row by the queue
/// key the Jobs panel (`queuePosition`) and `resume_on_launch` already share,
/// `ORDER BY created_at, id`. One reader for all three, so the panel can never
/// show an order the worker will not run.
pub async fn next_queued_job(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT id FROM render_job WHERE state = 'QUEUED' ORDER BY created_at, id LIMIT 1",
    )
    .fetch_optional(pool)
    .await
}

pub async fn update_job_state(
    pool: &SqlitePool,
    job_id: &str,
    state: &str,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "UPDATE render_job SET state = ?, updated_at = ? WHERE id = ?",
        state,
        now,
        job_id
    )
    .execute(pool)
    .await?;
    tracing::debug!(job = %job_id, "job state written: state={state}");
    Ok(())
}

pub async fn update_job_state_if(
    pool: &SqlitePool,
    job_id: &str,
    from: &str,
    to: &str,
) -> Result<bool, sqlx::Error> {
    let now = now_ms();
    let result = sqlx::query!(
        "UPDATE render_job SET state = ?, updated_at = ? WHERE id = ? AND state = ?",
        to,
        now,
        job_id,
        from
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_job_overnight(
    pool: &SqlitePool,
    job_id: &str,
    overnight: bool,
) -> Result<bool, sqlx::Error> {
    let now = now_ms();
    let result = sqlx::query("UPDATE render_job SET overnight = ?, updated_at = ? WHERE id = ?")
        .bind(overnight)
        .bind(now)
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn update_job_checkpoint(
    pool: &SqlitePool,
    job_id: &str,
    checkpoint_json: &str,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "UPDATE render_job SET checkpoint_json = ?, updated_at = ? WHERE id = ?",
        checkpoint_json,
        now,
        job_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_job_resource_plan(
    pool: &SqlitePool,
    job_id: &str,
    resource_plan_json: &str,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "UPDATE render_job SET resource_plan_json = ?, updated_at = ? WHERE id = ?",
        resource_plan_json,
        now,
        job_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub struct RecoveryWrite<'a> {
    pub column: PlanColumn,
    pub plan_json: &'a str,
    pub checkpoint_json: &'a str,
    pub state: &'a str,
    pub expected_state: &'a str,
    pub expected_rev: i64,
}

pub async fn apply_recovery(
    pool: &SqlitePool,
    job_id: &str,
    write: RecoveryWrite<'_>,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    let result = sqlx::query(&format!(
        "UPDATE render_job SET {} = ?, checkpoint_json = ?, state = ?, updated_at = ?
         WHERE id = ? AND state = ?
           AND COALESCE(json_extract(checkpoint_json, '$.resource_plan_rev'), 0) = ?",
        write.column.name()
    ))
    .bind(write.plan_json)
    .bind(write.checkpoint_json)
    .bind(write.state)
    .bind(now)
    .bind(job_id)
    .bind(write.expected_state)
    .bind(write.expected_rev)
    .execute(pool)
    .await?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(sqlx::Error::RowNotFound)
    }
}

pub async fn rewrite_plan_if_unchanged(
    pool: &SqlitePool,
    job_id: &str,
    column: PlanColumn,
    expected: &str,
    replacement: &str,
) -> Result<bool, sqlx::Error> {
    let now = now_ms();
    let rows = match column {
        PlanColumn::Resource => {
            sqlx::query!(
                "UPDATE render_job SET resource_plan_json = ?, updated_at = ?
                 WHERE id = ? AND resource_plan_json = ?",
                replacement,
                now,
                job_id,
                expected
            )
            .execute(pool)
            .await?
        }
        PlanColumn::Vision => {
            sqlx::query!(
                "UPDATE render_job SET vision_plan_json = ?, updated_at = ?
                 WHERE id = ? AND vision_plan_json = ?",
                replacement,
                now,
                job_id,
                expected
            )
            .execute(pool)
            .await?
        }
        PlanColumn::Tts => {
            sqlx::query!(
                "UPDATE render_job SET tts_plan_json = ?, updated_at = ?
                 WHERE id = ? AND tts_plan_json = ?",
                replacement,
                now,
                job_id,
                expected
            )
            .execute(pool)
            .await?
        }
        PlanColumn::Align => {
            sqlx::query!(
                "UPDATE render_job SET align_plan_json = ?, updated_at = ?
                 WHERE id = ? AND align_plan_json = ?",
                replacement,
                now,
                job_id,
                expected
            )
            .execute(pool)
            .await?
        }
    };
    Ok(rows.rows_affected() == 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanColumn {
    Resource,
    Vision,
    Tts,
    Align,
}

impl PlanColumn {
    pub fn name(self) -> &'static str {
        match self {
            PlanColumn::Resource => "resource_plan_json",
            PlanColumn::Vision => "vision_plan_json",
            PlanColumn::Tts => "tts_plan_json",
            PlanColumn::Align => "align_plan_json",
        }
    }
}

pub async fn set_job_vision_plan(
    pool: &SqlitePool,
    job_id: &str,
    vision_plan_json: &str,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "UPDATE render_job SET vision_plan_json = ?, updated_at = ? WHERE id = ?",
        vision_plan_json,
        now,
        job_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_job_tts_plan(
    pool: &SqlitePool,
    job_id: &str,
    tts_plan_json: &str,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "UPDATE render_job SET tts_plan_json = ?, updated_at = ? WHERE id = ?",
        tts_plan_json,
        now,
        job_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_job_align_plan(
    pool: &SqlitePool,
    job_id: &str,
    align_plan_json: &str,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "UPDATE render_job SET align_plan_json = ?, updated_at = ? WHERE id = ?",
        align_plan_json,
        now,
        job_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_job_report(
    pool: &SqlitePool,
    job_id: &str,
    report_json: &str,
    output_paths_json: &str,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "UPDATE render_job SET report_json = ?, output_paths_json = ?, updated_at = ? WHERE id = ?",
        report_json,
        output_paths_json,
        now,
        job_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_job_report_only(
    pool: &SqlitePool,
    job_id: &str,
    report_json: &str,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "UPDATE render_job SET report_json = ?, updated_at = ? WHERE id = ?",
        report_json,
        now,
        job_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn merge_report_value(
    pool: &SqlitePool,
    job_id: &str,
    key: &str,
    value: serde_json::Value,
) -> Result<(), sqlx::Error> {
    let Some(record) = get_job(pool, job_id).await? else {
        return Ok(());
    };
    let mut report: serde_json::Value = record
        .report_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    report[key] = value;
    set_job_report_only(pool, job_id, &report.to_string()).await
}

pub async fn insert_segments(
    pool: &SqlitePool,
    segments: &[ScriptSegment],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for segment in segments {
        sqlx::query!(
            "INSERT INTO script_segment (id, job_id, chapter_id, idx, page_no, kind, figure_id,
                                         text, source_span_json, flags_json, audio_hash, duration_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            segment.id,
            segment.job_id,
            segment.chapter_id,
            segment.idx,
            segment.page_no,
            segment.kind,
            segment.figure_id,
            segment.text,
            segment.source_span_json,
            segment.flags_json,
            segment.audio_hash,
            segment.duration_ms,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn list_segments(
    pool: &SqlitePool,
    job_id: &str,
) -> Result<Vec<ScriptSegment>, sqlx::Error> {
    sqlx::query_as!(
        ScriptSegment,
        r#"SELECT id as "id!", job_id, chapter_id, idx, page_no, kind, figure_id, text,
                  source_span_json, flags_json, audio_hash, duration_ms, original_text
           FROM script_segment WHERE job_id = ? ORDER BY idx"#,
        job_id
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuppressedBlock {
    pub id: String,
    pub job_id: String,
    pub page_no: i64,
    pub role: String,
    pub reason: String,
    pub certainty: String,
    pub text: String,
    pub words: i64,
}

pub async fn insert_suppressed_blocks(
    pool: &SqlitePool,
    blocks: &[SuppressedBlock],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for block in blocks {
        sqlx::query!(
            "INSERT INTO suppressed_block (id, job_id, page_no, role, reason, certainty, text, words)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            block.id,
            block.job_id,
            block.page_no,
            block.role,
            block.reason,
            block.certainty,
            block.text,
            block.words,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn list_suppressed_blocks(
    pool: &SqlitePool,
    job_id: &str,
) -> Result<Vec<SuppressedBlock>, sqlx::Error> {
    sqlx::query_as!(
        SuppressedBlock,
        r#"SELECT id as "id!", job_id, page_no, role, reason, certainty, text, words
           FROM suppressed_block WHERE job_id = ?
           ORDER BY CASE certainty WHEN 'uncertain' THEN 0 ELSE 1 END, page_no, id"#,
        job_id
    )
    .fetch_all(pool)
    .await
}

pub async fn delete_suppressed_blocks(pool: &SqlitePool, job_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM suppressed_block WHERE job_id = ?", job_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewSegment {
    pub id: String,
    pub chapter_id: String,
    pub idx: i64,
    pub page_no: i64,
    pub kind: String,
    pub text: String,
    pub flags_json: Option<String>,
    pub audio_hash: Option<String>,
    pub duration_ms: Option<i64>,
    pub original_text: Option<String>,
    pub wer: Option<f64>,
    pub align_source: Option<String>,
}

type ReviewRow = (
    String,
    String,
    i64,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<f64>,
    Option<String>,
);

pub async fn list_reviewable_segments(
    pool: &SqlitePool,
    job_id: &str,
    also_include: &[&str],
) -> Result<Vec<ReviewSegment>, sqlx::Error> {
    let placeholders = if also_include.is_empty() {
        "NULL".to_string()
    } else {
        vec!["?"; also_include.len()].join(", ")
    };
    let sql = format!(
        "SELECT s.id, s.chapter_id, s.idx, s.page_no, s.kind, s.text, s.flags_json,
                s.audio_hash, s.duration_ms, s.original_text, a.wer, a.source
         FROM script_segment s
         LEFT JOIN alignment a ON a.segment_id = s.id
         WHERE s.job_id = ?
           AND ( (s.flags_json IS NOT NULL AND s.flags_json NOT IN ('[]', ''))
                 OR s.original_text IS NOT NULL
                 OR s.id IN ({placeholders}) )
         ORDER BY s.idx"
    );
    let mut query = sqlx::query_as::<_, ReviewRow>(&sql).bind(job_id);
    for id in also_include {
        query = query.bind(*id);
    }
    let rows = query.fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| ReviewSegment {
            id: row.0,
            chapter_id: row.1,
            idx: row.2,
            page_no: row.3,
            kind: row.4,
            text: row.5,
            flags_json: row.6,
            audio_hash: row.7,
            duration_ms: row.8,
            original_text: row.9,
            wer: row.10,
            align_source: row.11,
        })
        .collect())
}

pub async fn set_segment_audio(
    pool: &SqlitePool,
    segment_id: &str,
    audio_hash: &str,
    duration_ms: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE script_segment SET audio_hash = ?, duration_ms = ? WHERE id = ?",
        audio_hash,
        duration_ms,
        segment_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_segment_flags(
    pool: &SqlitePool,
    segment_id: &str,
    flags_json: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE script_segment SET flags_json = ? WHERE id = ?",
        flags_json,
        segment_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn edit_segment_text(
    pool: &SqlitePool,
    segment_id: &str,
    text: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "UPDATE script_segment
            SET original_text = COALESCE(original_text, text),
                text = ?,
                audio_hash = NULL,
                duration_ms = NULL
          WHERE id = ? AND text <> ?",
        text,
        segment_id,
        text
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn revert_segment_text(pool: &SqlitePool, segment_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        "UPDATE script_segment
            SET text = original_text,
                original_text = NULL,
                audio_hash = NULL,
                duration_ms = NULL
          WHERE id = ? AND original_text IS NOT NULL",
        segment_id
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete_alignment(pool: &SqlitePool, segment_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM alignment WHERE segment_id = ?", segment_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn clear_segment_audio(pool: &SqlitePool, segment_id: &str) -> Result<(), sqlx::Error> {
    clear_segments_audio(pool, &[segment_id]).await
}

pub async fn clear_segments_audio(
    pool: &SqlitePool,
    segment_ids: &[&str],
) -> Result<(), sqlx::Error> {
    if segment_ids.is_empty() {
        return Ok(());
    }
    for chunk in segment_ids.chunks(256) {
        let mut tx = pool.begin().await?;
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql_segment = format!(
            "UPDATE script_segment SET audio_hash = NULL, duration_ms = NULL WHERE id IN ({placeholders})"
        );
        let mut q_seg = sqlx::query(&sql_segment);
        for id in chunk {
            q_seg = q_seg.bind(id);
        }
        q_seg.execute(&mut *tx).await?;

        let sql_align = format!("DELETE FROM alignment WHERE segment_id IN ({placeholders})");
        let mut q_align = sqlx::query(&sql_align);
        for id in chunk {
            q_align = q_align.bind(id);
        }
        q_align.execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(())
}

pub async fn delete_chapter_renders_for_job(
    pool: &SqlitePool,
    job_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM chapter_render WHERE job_id = ?")
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn reconcile_purged_cache_jobs(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let seg_res = sqlx::query(
        "UPDATE script_segment SET audio_hash = NULL, duration_ms = NULL \
         WHERE job_id IN (SELECT id FROM render_job WHERE UPPER(state) NOT IN ('DONE')) \
           AND audio_hash IS NOT NULL",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "DELETE FROM alignment WHERE segment_id IN ( \
             SELECT id FROM script_segment WHERE job_id IN ( \
                 SELECT id FROM render_job WHERE UPPER(state) NOT IN ('DONE') \
             ) \
         )",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "DELETE FROM chapter_render WHERE job_id IN ( \
             SELECT id FROM render_job WHERE UPPER(state) NOT IN ('DONE') \
         )",
    )
    .execute(&mut *tx)
    .await?;

    let rows = sqlx::query(
        "SELECT id, checkpoint_json FROM render_job WHERE UPPER(state) NOT IN ('DONE') AND checkpoint_json IS NOT NULL",
    )
    .fetch_all(&mut *tx)
    .await?;

    for row in rows {
        let job_id: String = row.get(0);
        let raw: Option<String> = row.get(1);
        if let Some(raw) = raw {
            if let Ok(mut cp) = serde_json::from_str::<serde_json::Value>(&raw) {
                let stage_str = cp
                    .get("stage")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_uppercase();
                if matches!(
                    stage_str.as_str(),
                    "SYNTHESIZING" | "ALIGNING" | "RENDERING"
                ) {
                    cp["stage"] = serde_json::json!("SYNTHESIZING");
                    cp["done_segments"] = serde_json::json!(0);
                    let updated = serde_json::to_string(&cp).unwrap_or(raw);
                    sqlx::query("UPDATE render_job SET checkpoint_json = ? WHERE id = ?")
                        .bind(updated)
                        .bind(&job_id)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
    }

    tx.commit().await?;
    Ok(seg_res.rows_affected())
}

pub async fn clear_checkpoint(pool: &SqlitePool, job_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE render_job SET checkpoint_json = NULL WHERE id = ?",
        job_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn upsert_alignment(pool: &SqlitePool, alignment: &Alignment) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO alignment (segment_id, source, words_json, wer)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(segment_id) DO UPDATE SET source = excluded.source,
                                               words_json = excluded.words_json,
                                               wer = excluded.wer",
        alignment.segment_id,
        alignment.source,
        alignment.words_json,
        alignment.wer,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_alignments(
    pool: &SqlitePool,
    job_id: &str,
) -> Result<Vec<Alignment>, sqlx::Error> {
    sqlx::query_as!(
        Alignment,
        r#"SELECT a.segment_id as "segment_id!", a.source,
                  a.words_json as "words_json: WordsJson", a.wer
           FROM alignment a
           JOIN script_segment s ON s.id = a.segment_id
           WHERE s.job_id = ?
           ORDER BY s.idx"#,
        job_id
    )
    .fetch_all(pool)
    .await
}

pub async fn get_alignment(
    pool: &SqlitePool,
    segment_id: &str,
) -> Result<Option<Alignment>, sqlx::Error> {
    sqlx::query_as!(
        Alignment,
        r#"SELECT segment_id as "segment_id!", source,
                  words_json as "words_json: WordsJson", wer
           FROM alignment WHERE segment_id = ?"#,
        segment_id
    )
    .fetch_optional(pool)
    .await
}

/// One durable completed-chunk record for the cloud Script path (S-3b,
/// Hermes MF11): the provider-neutral outcomes a source chunk produced
/// (one, or two after halving) plus the rolling summary AFTER it — enough
/// to resume without re-requesting any completed chunk.
#[derive(Debug, Clone)]
pub struct ScriptChunkCheckpoint {
    pub chunk_idx: i64,
    pub units_fingerprint: String,
    pub total_chunks: i64,
    pub outcome_json: String,
    pub summary: String,
}

pub async fn upsert_script_chunk_checkpoint(
    pool: &SqlitePool,
    job_id: &str,
    checkpoint: &ScriptChunkCheckpoint,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO script_chunk_checkpoint \
         (job_id, chunk_idx, units_fingerprint, total_chunks, outcome_json, summary, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(job_id, chunk_idx) DO UPDATE SET \
         units_fingerprint = excluded.units_fingerprint, \
         total_chunks = excluded.total_chunks, \
         outcome_json = excluded.outcome_json, \
         summary = excluded.summary",
    )
    .bind(job_id)
    .bind(checkpoint.chunk_idx)
    .bind(&checkpoint.units_fingerprint)
    .bind(checkpoint.total_chunks)
    .bind(&checkpoint.outcome_json)
    .bind(&checkpoint.summary)
    .bind(crate::storage::now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_script_chunk_checkpoints(
    pool: &SqlitePool,
    job_id: &str,
) -> Result<Vec<ScriptChunkCheckpoint>, sqlx::Error> {
    let rows: Vec<(i64, String, i64, String, String)> = sqlx::query_as(
        "SELECT chunk_idx, units_fingerprint, total_chunks, outcome_json, summary \
         FROM script_chunk_checkpoint WHERE job_id = ? ORDER BY chunk_idx",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(chunk_idx, units_fingerprint, total_chunks, outcome_json, summary)| {
                ScriptChunkCheckpoint {
                    chunk_idx,
                    units_fingerprint,
                    total_chunks,
                    outcome_json,
                    summary,
                }
            },
        )
        .collect())
}

pub async fn delete_script_chunk_checkpoints(
    pool: &SqlitePool,
    job_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM script_chunk_checkpoint WHERE job_id = ?")
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// One request-identity cache row (S-4, Hermes MF6): the audio facts an
/// identical provider request already paid for, including admitted native
/// timing so a cache hit reproduces the alignment too.
#[derive(Debug, Clone)]
pub struct ProviderRequestCacheRow {
    pub audio_hash: String,
    pub wav_bytes: i64,
    pub duration_ms: i64,
    pub words_json: Option<String>,
}

pub async fn get_provider_request_cache(
    pool: &SqlitePool,
    request_key: &str,
) -> Result<Option<ProviderRequestCacheRow>, sqlx::Error> {
    let row: Option<(String, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT audio_hash, wav_bytes, duration_ms, words_json \
         FROM provider_request_cache WHERE request_key = ?",
    )
    .bind(request_key)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(audio_hash, wav_bytes, duration_ms, words_json)| ProviderRequestCacheRow {
            audio_hash,
            wav_bytes,
            duration_ms,
            words_json,
        },
    ))
}

pub async fn upsert_provider_request_cache(
    pool: &SqlitePool,
    request_key: &str,
    row: &ProviderRequestCacheRow,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO provider_request_cache \
         (request_key, audio_hash, wav_bytes, duration_ms, words_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(request_key) DO UPDATE SET audio_hash = excluded.audio_hash, \
         wav_bytes = excluded.wav_bytes, duration_ms = excluded.duration_ms, \
         words_json = excluded.words_json",
    )
    .bind(request_key)
    .bind(&row.audio_hash)
    .bind(row.wav_bytes)
    .bind(row.duration_ms)
    .bind(&row.words_json)
    .bind(crate::storage::now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

/// Atomic paid-synthesis settlement (Hermes R2 fold-in 6): segment audio
/// facts, cache ledger entry + job ref, optional native alignment, and the
/// request-identity mapping commit in ONE transaction — a crash after a
/// paid response can no longer strand audio without its mapping (forcing a
/// re-payment) or a segment fact without its ledger row.
#[allow(clippy::too_many_arguments)]
pub async fn settle_cloud_segment(
    pool: &SqlitePool,
    job_id: &str,
    segment_id: &str,
    audio_hash: &str,
    wav_bytes: i64,
    duration_ms: i64,
    alignment_words_json: Option<&str>,
    request_key: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE script_segment SET audio_hash = ?, duration_ms = ? WHERE id = ?")
        .bind(audio_hash)
        .bind(duration_ms)
        .bind(segment_id)
        .execute(&mut *tx)
        .await?;
    let now = crate::storage::now_ms();
    sqlx::query(
        "INSERT INTO cache_entry (hash, kind, bytes, last_used_at) VALUES (?, 'wav', ?, ?) \
         ON CONFLICT(hash) DO UPDATE SET kind = excluded.kind, bytes = excluded.bytes, \
         last_used_at = excluded.last_used_at",
    )
    .bind(audio_hash)
    .bind(wav_bytes)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO cache_ref (hash, job_id) VALUES (?, ?) ON CONFLICT(hash, job_id) DO NOTHING",
    )
    .bind(audio_hash)
    .bind(job_id)
    .execute(&mut *tx)
    .await?;
    if let Some(words_json) = alignment_words_json {
        sqlx::query(
            "INSERT INTO alignment (segment_id, source, words_json, wer) VALUES (?, ?, ?, NULL) \
             ON CONFLICT(segment_id) DO UPDATE SET source = excluded.source, \
             words_json = excluded.words_json, wer = excluded.wer",
        )
        .bind(segment_id)
        .bind(crate::words::SOURCE_TTS_NATIVE)
        .bind(words_json)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO provider_request_cache \
         (request_key, audio_hash, wav_bytes, duration_ms, words_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(request_key) DO UPDATE SET audio_hash = excluded.audio_hash, \
         wav_bytes = excluded.wav_bytes, duration_ms = excluded.duration_ms, \
         words_json = excluded.words_json",
    )
    .bind(request_key)
    .bind(audio_hash)
    .bind(wav_bytes)
    .bind(duration_ms)
    .bind(alignment_words_json)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentTextEdit<'a> {
    Replace(&'a str),
    Revert,
}

#[allow(clippy::too_many_arguments)]
pub async fn settle_local_segment(
    pool: &SqlitePool,
    job_id: &str,
    segment_id: &str,
    audio_hash: &str,
    wav_bytes: i64,
    duration_ms: i64,
    alignment_words_json: Option<&str>,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let updated =
        sqlx::query("UPDATE script_segment SET audio_hash = ?, duration_ms = ? WHERE id = ?")
            .bind(audio_hash)
            .bind(duration_ms)
            .bind(segment_id)
            .execute(&mut *tx)
            .await?;
    if updated.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }
    let now = crate::storage::now_ms();
    sqlx::query(
        "INSERT INTO cache_entry (hash, kind, bytes, last_used_at) VALUES (?, 'wav', ?, ?) \
         ON CONFLICT(hash) DO UPDATE SET kind = excluded.kind, bytes = excluded.bytes, \
         last_used_at = excluded.last_used_at",
    )
    .bind(audio_hash)
    .bind(wav_bytes)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO cache_ref (hash, job_id) VALUES (?, ?) ON CONFLICT(hash, job_id) DO NOTHING",
    )
    .bind(audio_hash)
    .bind(job_id)
    .execute(&mut *tx)
    .await?;
    if let Some(words_json) = alignment_words_json {
        sqlx::query(
            "INSERT INTO alignment (segment_id, source, words_json, wer) VALUES (?, ?, ?, NULL) \
             ON CONFLICT(segment_id) DO UPDATE SET source = excluded.source, \
             words_json = excluded.words_json, wer = excluded.wer",
        )
        .bind(segment_id)
        .bind(crate::words::SOURCE_TTS_NATIVE)
        .bind(words_json)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn resynthesize_segment_reset(
    pool: &SqlitePool,
    job_id: &str,
    segment_id: &str,
    chapter_id: &str,
    text_edit: SegmentTextEdit<'_>,
    flags_json: Option<&str>,
    checkpoint_json: &str,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let now = crate::storage::now_ms();

    let changed = match text_edit {
        SegmentTextEdit::Replace(trimmed) => {
            let res = sqlx::query(
                "UPDATE script_segment \
                    SET original_text = COALESCE(original_text, text), \
                        text = ?, \
                        audio_hash = NULL, \
                        duration_ms = NULL, \
                        flags_json = COALESCE(?, flags_json) \
                  WHERE id = ? AND text <> ?",
            )
            .bind(trimmed)
            .bind(flags_json)
            .bind(segment_id)
            .bind(trimmed)
            .execute(&mut *tx)
            .await?;
            res.rows_affected() > 0
        }
        SegmentTextEdit::Revert => {
            let res = sqlx::query(
                "UPDATE script_segment \
                    SET text = original_text, \
                        original_text = NULL, \
                        audio_hash = NULL, \
                        duration_ms = NULL, \
                        flags_json = COALESCE(?, flags_json) \
                  WHERE id = ? AND original_text IS NOT NULL",
            )
            .bind(flags_json)
            .bind(segment_id)
            .execute(&mut *tx)
            .await?;
            res.rows_affected() > 0
        }
    };

    if !changed {
        return Ok(false);
    }

    sqlx::query("DELETE FROM alignment WHERE segment_id = ?")
        .bind(segment_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM chapter_render WHERE job_id = ? AND chapter_id = ?")
        .bind(job_id)
        .bind(chapter_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "UPDATE render_job \
            SET checkpoint_json = ?, \
                state = 'QUEUED', \
                updated_at = ? \
          WHERE id = ?",
    )
    .bind(checkpoint_json)
    .bind(now)
    .bind(job_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(true)
}

pub async fn delete_provider_request_cache(
    pool: &SqlitePool,
    request_key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM provider_request_cache WHERE request_key = ?")
        .bind(request_key)
        .execute(pool)
        .await?;
    Ok(())
}

/// GC hook (fold-in 6): when a wav is evicted from the content cache, its
/// request mappings must die too, or dead rows accumulate forever.
pub async fn delete_request_cache_for_audio(
    pool: &SqlitePool,
    audio_hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM provider_request_cache WHERE audio_hash = ?")
        .bind(audio_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Atomic FAILED-job recovery to Local (Hermes R2 blocker 7): the binding
/// strip, recovery fact, and state change land in ONE conditional UPDATE —
/// the WHERE clause is the FAILED validation, so nothing mutates unless
/// the job really is failed, and nothing half-mutates.
pub async fn recover_failed_job_to_local(
    pool: &SqlitePool,
    job_id: &str,
    model_overrides_json: Option<&str>,
    reset_throttle: bool,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE render_job SET state = 'QUEUED', model_overrides_json = ?, \
         checkpoint_json = CASE WHEN ? AND json_valid(checkpoint_json) \
         THEN json_set(checkpoint_json, '$.rate_limit_pauses', 0, '$.last_pause_done', 0) \
         ELSE checkpoint_json END, updated_at = ? \
         WHERE id = ? AND state = 'FAILED'",
    )
    .bind(model_overrides_json)
    .bind(reset_throttle)
    .bind(crate::storage::now_ms())
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// THE atomic terminal settlement write (Hermes attempt-4 design): state
/// and final report land in ONE UPDATE — there is no crash window between
/// a terminal state commit and its report merge, and an optional expected
/// state makes the write race-safe for transition semantics.
pub async fn settle_terminal(
    pool: &SqlitePool,
    job_id: &str,
    state: &str,
    report_json: Option<&str>,
    expected_state: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let now = crate::storage::now_ms();
    let result = match expected_state {
        Some(expected) => {
            sqlx::query(
                "UPDATE render_job SET state = ?, report_json = COALESCE(?, report_json),                  updated_at = ? WHERE id = ? AND state = ?",
            )
            .bind(state)
            .bind(report_json)
            .bind(now)
            .bind(job_id)
            .bind(expected)
            .execute(pool)
            .await?
        }
        None => {
            sqlx::query(
                "UPDATE render_job SET state = ?, report_json = COALESCE(?, report_json),                  updated_at = ? WHERE id = ?",
            )
            .bind(state)
            .bind(report_json)
            .bind(now)
            .bind(job_id)
            .execute(pool)
            .await?
        }
    };
    Ok(result.rows_affected() == 1)
}

/// The sha256 hash of the 250ms silence WAV previously generated by the legacy fallback path.
pub const POISONED_SILENCE_AUDIO_HASH: &str =
    "9623afc24a77f9112c0fe5fbece3f9eac2f9a67b504a1d5918149c3974dff188";

/// Purge poisoned provider_request_cache rows mapped to the legacy 250ms silence WAV.
/// Idempotent and safe to run multiple times.
pub async fn purge_silence_fallback_cache(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM provider_request_cache WHERE audio_hash = ?")
        .bind(POISONED_SILENCE_AUDIO_HASH)
        .execute(pool)
        .await?;
    let count = result.rows_affected();
    if count > 0 {
        tracing::info!(
            rows_purged = count,
            audio_hash = POISONED_SILENCE_AUDIO_HASH,
            "purged poisoned silence-fallback rows from provider_request_cache"
        );
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn purge_silence_fallback_cache_removes_poisoned_rows_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        let poisoned_row = ProviderRequestCacheRow {
            audio_hash: POISONED_SILENCE_AUDIO_HASH.to_string(),
            wav_bytes: 100,
            duration_ms: 250,
            words_json: None,
        };
        let good_row = ProviderRequestCacheRow {
            audio_hash: "good-hash-456".to_string(),
            wav_bytes: 2000,
            duration_ms: 1500,
            words_json: Some("[]".to_string()),
        };

        upsert_provider_request_cache(&pool, "req-poisoned", &poisoned_row)
            .await
            .unwrap();
        upsert_provider_request_cache(&pool, "req-good", &good_row)
            .await
            .unwrap();

        let purged = purge_silence_fallback_cache(&pool).await.unwrap();
        assert_eq!(purged, 1);

        assert!(get_provider_request_cache(&pool, "req-poisoned")
            .await
            .unwrap()
            .is_none());
        assert!(get_provider_request_cache(&pool, "req-good")
            .await
            .unwrap()
            .is_some());

        let purged_second = purge_silence_fallback_cache(&pool).await.unwrap();
        assert_eq!(purged_second, 0);

        assert!(get_provider_request_cache(&pool, "req-good")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn active_render_jobs_query_excludes_finished_states_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(
            "INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        for (id, state) in [
            ("j1", "DONE"),
            ("j2", "FAILED"),
            ("j3", "CANCELLED"),
            ("j4", "done"),
            ("j5", "failed"),
            ("j6", "canceled"),
        ] {
            sqlx::query(
                "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                 output_format, state, created_at, updated_at) \
                 VALUES (?, 'b1', '{}', 'teacher', 'vp1', 'mp3', ?, 1, 1)",
            )
            .bind(id)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
        }

        let active_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM render_job WHERE UPPER(state) NOT IN ('DONE', 'FAILED', 'CANCELLED', 'CANCELED')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            active_count, 0,
            "All terminal states must be excluded from active count"
        );

        sqlx::raw_sql(
            "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
             output_format, state, created_at, updated_at) \
             VALUES ('j7', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'SYNTHESIZING', 1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let active_count_after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM render_job WHERE UPPER(state) NOT IN ('DONE', 'FAILED', 'CANCELLED', 'CANCELED')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active_count_after, 1, "Active job must be counted");
    }

    #[tokio::test]
    async fn settle_local_segment_atomically_updates_segment_cache_and_alignment() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO chapter (id, book_id, idx, title, page_start, page_end) VALUES ('c1', 'b1', 0, 'Ch1', 1, 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, created_at, updated_at) \
             VALUES ('j1', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'SYNTHESIZING', 1, 1); \
             INSERT INTO script_segment (id, job_id, chapter_id, idx, page_no, kind, text) \
             VALUES ('s1', 'j1', 'c1', 0, 1, 'text', 'Hello world');",
        )
        .execute(&pool)
        .await
        .unwrap();

        let words_json =
            r#"[{"word":"Hello","start":0.0,"end":0.5},{"word":"world","start":0.5,"end":1.0}]"#;
        settle_local_segment(
            &pool,
            "j1",
            "s1",
            "hash-wav-123",
            4096,
            1000,
            Some(words_json),
        )
        .await
        .unwrap();

        let segment = list_segments(&pool, "j1")
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == "s1")
            .unwrap();
        assert_eq!(segment.audio_hash.as_deref(), Some("hash-wav-123"));
        assert_eq!(segment.duration_ms, Some(1000));

        let cache = crate::storage::cache::get_entry(&pool, "hash-wav-123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cache.kind, "wav");
        assert_eq!(cache.bytes, 4096);

        let ref_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM cache_ref WHERE hash = 'hash-wav-123' AND job_id = 'j1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ref_count, 1);

        let alignment = get_alignment(&pool, "s1").await.unwrap().unwrap();
        assert_eq!(alignment.source, crate::words::SOURCE_TTS_NATIVE);
        assert_eq!(alignment.words_json.as_str(), words_json);
    }

    #[tokio::test]
    async fn settle_local_segment_without_alignment_skips_alignment_table() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO chapter (id, book_id, idx, title, page_start, page_end) VALUES ('c1', 'b1', 0, 'Ch1', 1, 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, created_at, updated_at) \
             VALUES ('j1', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'SYNTHESIZING', 1, 1); \
             INSERT INTO script_segment (id, job_id, chapter_id, idx, page_no, kind, text) \
             VALUES ('s1', 'j1', 'c1', 0, 1, 'text', 'Hello world');",
        )
        .execute(&pool)
        .await
        .unwrap();

        settle_local_segment(&pool, "j1", "s1", "hash-wav-no-align", 2048, 500, None)
            .await
            .unwrap();

        let alignment = get_alignment(&pool, "s1").await.unwrap();
        assert!(alignment.is_none());
    }

    #[tokio::test]
    async fn settle_local_segment_missing_segment_returns_row_not_found_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        let result =
            settle_local_segment(&pool, "j1", "s_missing", "hash-bad", 1024, 300, None).await;
        assert!(matches!(result, Err(sqlx::Error::RowNotFound)));

        let cache = crate::storage::cache::get_entry(&pool, "hash-bad")
            .await
            .unwrap();
        assert!(cache.is_none());
    }

    #[tokio::test]
    async fn resynthesize_segment_reset_atomically_clears_audio_alignment_chapter_and_requeues_job()
    {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO chapter (id, book_id, idx, title, page_start, page_end) VALUES ('c1', 'b1', 0, 'Ch1', 1, 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, checkpoint_json, created_at, updated_at) \
             VALUES ('j1', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'DONE', '{\"stage\":\"Done\"}', 1, 1); \
             INSERT INTO script_segment (id, job_id, chapter_id, idx, page_no, kind, text, audio_hash, duration_ms, flags_json) \
             VALUES ('s1', 'j1', 'c1', 0, 1, 'text', 'Old text', 'old-hash', 800, '[\"verify_failed\"]'); \
             INSERT INTO alignment (segment_id, source, words_json, wer) \
             VALUES ('s1', 'tts_native', '[]', 0.1); \
             INSERT INTO chapter_render (job_id, chapter_id, idx, hash, artifact, duration_ms, encoded_ms) \
             VALUES ('j1', 'c1', 0, 'h1', 'mp3', 800, 800);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let changed = resynthesize_segment_reset(
            &pool,
            "j1",
            "s1",
            "c1",
            SegmentTextEdit::Replace("New edited text"),
            Some("[]"),
            r#"{"stage":"Synthesizing"}"#,
        )
        .await
        .unwrap();

        assert!(changed);

        let segment = list_segments(&pool, "j1")
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == "s1")
            .unwrap();
        assert_eq!(segment.text, "New edited text");
        assert_eq!(segment.original_text.as_deref(), Some("Old text"));
        assert_eq!(segment.audio_hash, None);
        assert_eq!(segment.duration_ms, None);
        assert_eq!(segment.flags_json.as_deref(), Some("[]"));

        let alignment = get_alignment(&pool, "s1").await.unwrap();
        assert!(alignment.is_none());

        let chapter_render: Option<String> = sqlx::query_scalar(
            "SELECT artifact FROM chapter_render WHERE job_id = 'j1' AND chapter_id = 'c1'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(chapter_render.is_none());

        let job = get_job(&pool, "j1").await.unwrap().unwrap();
        assert_eq!(job.state, "QUEUED");
        assert_eq!(
            job.checkpoint_json.as_deref(),
            Some(r#"{"stage":"Synthesizing"}"#)
        );
    }

    #[tokio::test]
    async fn resynthesize_segment_reset_rolls_back_if_text_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO chapter (id, book_id, idx, title, page_start, page_end) VALUES ('c1', 'b1', 0, 'Ch1', 1, 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, created_at, updated_at) \
             VALUES ('j1', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'DONE', 1, 1); \
             INSERT INTO script_segment (id, job_id, chapter_id, idx, page_no, kind, text, audio_hash, duration_ms) \
             VALUES ('s1', 'j1', 'c1', 0, 1, 'text', 'Same text', 'hash-1', 500);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let changed = resynthesize_segment_reset(
            &pool,
            "j1",
            "s1",
            "c1",
            SegmentTextEdit::Replace("Same text"),
            None,
            r#"{"stage":"Synthesizing"}"#,
        )
        .await
        .unwrap();

        assert!(!changed);

        let segment = list_segments(&pool, "j1")
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == "s1")
            .unwrap();
        assert_eq!(segment.audio_hash.as_deref(), Some("hash-1"));
        let job = get_job(&pool, "j1").await.unwrap().unwrap();
        assert_eq!(job.state, "DONE");
    }

    #[tokio::test]
    async fn resynthesize_segment_reset_reverts_text_and_clears_original_text() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("test.db"))
            .await
            .unwrap();

        sqlx::raw_sql(
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at) \
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1); \
             INSERT INTO chapter (id, book_id, idx, title, page_start, page_end) VALUES ('c1', 'b1', 0, 'Ch1', 1, 1); \
             INSERT INTO voice_profile (id, name, engine, kind, created_at) \
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1); \
             INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id, \
                                     output_format, state, created_at, updated_at) \
             VALUES ('j1', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'DONE', 1, 1); \
             INSERT INTO script_segment (id, job_id, chapter_id, idx, page_no, kind, text, original_text, audio_hash, duration_ms) \
             VALUES ('s1', 'j1', 'c1', 0, 1, 'text', 'Edited text', 'Original text', 'hash-1', 500);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let changed = resynthesize_segment_reset(
            &pool,
            "j1",
            "s1",
            "c1",
            SegmentTextEdit::Revert,
            None,
            r#"{"stage":"Synthesizing"}"#,
        )
        .await
        .unwrap();

        assert!(changed);

        let segment = list_segments(&pool, "j1")
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == "s1")
            .unwrap();
        assert_eq!(segment.text, "Original text");
        assert_eq!(segment.original_text, None);
        assert_eq!(segment.audio_hash, None);
    }
}
