use std::collections::HashMap;

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

#[derive(Debug, Clone, PartialEq)]
pub struct ChapterRender {
    pub job_id: String,
    pub chapter_id: String,
    pub idx: i64,
    pub hash: String,
    pub artifact: String,
    pub duration_ms: i64,
    pub encoded_ms: Option<i64>,
}

pub const ARTIFACT_MP3: &str = "mp3";
pub const ARTIFACT_NONE: &str = "none";

pub fn chapter_hash(
    audio_hashes: &[String],
    segment_gap_ms: i64,
    chapter_gap_ms: i64,
    encoder_signature: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"chapter_render:v1");
    for audio_hash in audio_hashes {
        hasher.update(b"\x1f");
        hasher.update(audio_hash.as_bytes());
    }
    hasher.update(format!("\x1egaps:{segment_gap_ms}:{chapter_gap_ms}").as_bytes());
    hasher.update(format!("\x1eencoder:{encoder_signature}").as_bytes());
    hex::encode(hasher.finalize())
}

pub fn digest(rows: &[ChapterRender]) -> String {
    let mut ordered: Vec<&ChapterRender> = rows.iter().collect();
    ordered.sort_by_key(|row| row.idx);
    let mut hasher = Sha256::new();
    hasher.update(b"chapter_set:v1");
    for row in ordered {
        hasher.update(format!("\x1e{}\x1f{}\x1f", row.idx, row.chapter_id).as_bytes());
        hasher.update(row.hash.as_bytes());
    }
    hex::encode(hasher.finalize())
}

pub fn artifact_key(chapter_hash: &str, loudnorm_filter: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"chapter_artifact:v1");
    hasher.update(chapter_hash.as_bytes());
    hasher.update(b"\x1e");
    hasher.update(loudnorm_filter.as_bytes());
    hex::encode(hasher.finalize())
}

pub async fn replace_all(
    pool: &SqlitePool,
    job_id: &str,
    rows: &[ChapterRender],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query!("DELETE FROM chapter_render WHERE job_id = ?", job_id)
        .execute(&mut *tx)
        .await?;
    for row in rows {
        sqlx::query!(
            "INSERT INTO chapter_render (job_id, chapter_id, idx, hash, artifact, duration_ms,
                                         encoded_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            row.job_id,
            row.chapter_id,
            row.idx,
            row.hash,
            row.artifact,
            row.duration_ms,
            row.encoded_ms,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn list(pool: &SqlitePool, job_id: &str) -> Result<Vec<ChapterRender>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT job_id as "job_id!", chapter_id as "chapter_id!", idx as "idx!",
                  hash as "hash!", artifact as "artifact!", duration_ms as "duration_ms!",
                  encoded_ms
           FROM chapter_render WHERE job_id = ? ORDER BY idx"#,
        job_id
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ChapterRender {
            job_id: row.job_id,
            chapter_id: row.chapter_id,
            idx: row.idx,
            hash: row.hash,
            artifact: row.artifact,
            duration_ms: row.duration_ms,
            encoded_ms: row.encoded_ms,
        })
        .collect())
}

pub async fn digests_for_jobs(
    pool: &SqlitePool,
    job_ids: &[&str],
) -> Result<HashMap<String, String>, sqlx::Error> {
    if job_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; job_ids.len()].join(", ");
    let sql = format!(
        "SELECT job_id, chapter_id, idx, hash, artifact, duration_ms, encoded_ms
         FROM chapter_render WHERE job_id IN ({placeholders}) ORDER BY job_id, idx"
    );
    let mut query =
        sqlx::query_as::<_, (String, String, i64, String, String, i64, Option<i64>)>(&sql);
    for job_id in job_ids {
        query = query.bind(*job_id);
    }
    let rows = query.fetch_all(pool).await?;
    let mut grouped: HashMap<String, Vec<ChapterRender>> = HashMap::new();
    for (job_id, chapter_id, idx, hash, artifact, duration_ms, encoded_ms) in rows {
        grouped
            .entry(job_id.clone())
            .or_default()
            .push(ChapterRender {
                job_id,
                chapter_id,
                idx,
                hash,
                artifact,
                duration_ms,
                encoded_ms,
            });
    }
    Ok(grouped
        .into_iter()
        .map(|(job_id, rows)| (job_id, digest(&rows)))
        .collect())
}

pub async fn delete(pool: &SqlitePool, job_id: &str, chapter_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "DELETE FROM chapter_render WHERE job_id = ? AND chapter_id = ?",
        job_id,
        chapter_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIGNATURE: &str =
        "-c:a libmp3lame -q:a 0 -f mp3|sr=24000|ffmpeg7.1.5+libavcodec61.19.101";

    fn hashes(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn the_hash_covers_every_chapter_local_input() {
        let base = chapter_hash(&hashes(&["a", "b"]), 400, 1200, SIGNATURE);
        assert_ne!(
            base,
            chapter_hash(&hashes(&["a", "c"]), 400, 1200, SIGNATURE)
        );
        assert_ne!(
            base,
            chapter_hash(&hashes(&["b", "a"]), 400, 1200, SIGNATURE)
        );
        assert_ne!(
            base,
            chapter_hash(&hashes(&["a", "b"]), 300, 1200, SIGNATURE)
        );
        assert_ne!(
            base,
            chapter_hash(&hashes(&["a", "b"]), 400, 900, SIGNATURE)
        );
        assert_ne!(
            base,
            chapter_hash(
                &hashes(&["a", "b"]),
                400,
                1200,
                "libmp3lame -q:a 2|sr=24000|ffmpeg7.1.5"
            ),
            "a different encoder must not reuse another encoder's bytes"
        );
    }

    #[test]
    fn the_hash_ignores_the_book_wide_loudness_measurement() {
        let chapter = chapter_hash(&hashes(&["a", "b"]), 400, 1200, SIGNATURE);
        assert_eq!(
            chapter,
            chapter_hash(&hashes(&["a", "b"]), 400, 1200, SIGNATURE)
        );
        assert_ne!(
            artifact_key(&chapter, "loudnorm=measured_I=-24.92"),
            artifact_key(&chapter, "loudnorm=measured_I=-23.10"),
            "the bytes depend on the measurement even though the chapter does not"
        );
    }

    #[test]
    fn the_hash_is_stable_across_runs() {
        assert_eq!(
            chapter_hash(&hashes(&["a", "b"]), 400, 1200, SIGNATURE),
            chapter_hash(&hashes(&["a", "b"]), 400, 1200, SIGNATURE)
        );
    }

    #[test]
    fn a_segment_boundary_cannot_be_forged_by_concatenation() {
        assert_ne!(
            chapter_hash(&hashes(&["ab", "c"]), 400, 1200, SIGNATURE),
            chapter_hash(&hashes(&["a", "bc"]), 400, 1200, SIGNATURE)
        );
    }
}
