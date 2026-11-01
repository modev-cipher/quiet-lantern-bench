use sqlx::SqlitePool;

use super::now_ms;

#[derive(Debug, Clone, PartialEq)]
pub struct CacheEntry {
    pub hash: String,
    pub kind: String,
    pub bytes: i64,
    pub last_used_at: i64,
}

pub async fn upsert_entry(
    pool: &SqlitePool,
    hash: &str,
    kind: &str,
    bytes: i64,
) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "INSERT INTO cache_entry (hash, kind, bytes, last_used_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(hash) DO UPDATE SET kind = excluded.kind,
                                         bytes = excluded.bytes,
                                         last_used_at = excluded.last_used_at",
        hash,
        kind,
        bytes,
        now,
    )
    .execute(pool)
    .await?;
    tracing::debug!("cache write: key={hash} kind={kind} bytes={bytes}");
    Ok(())
}

pub async fn record_use(
    pool: &SqlitePool,
    hash: &str,
    kind: &str,
    bytes: i64,
    job_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    upsert_entry(pool, hash, kind, bytes).await?;
    if let Some(job_id) = job_id {
        sqlx::query!(
            "INSERT INTO cache_ref (hash, job_id) VALUES (?, ?)
             ON CONFLICT(hash, job_id) DO NOTHING",
            hash,
            job_id
        )
        .execute(pool)
        .await?;
    }
    Ok(())
}

pub async fn get_entry(pool: &SqlitePool, hash: &str) -> Result<Option<CacheEntry>, sqlx::Error> {
    let entry = sqlx::query_as!(
        CacheEntry,
        r#"SELECT hash as "hash!", kind, bytes, last_used_at
           FROM cache_entry WHERE hash = ?"#,
        hash
    )
    .fetch_optional(pool)
    .await?;
    match &entry {
        Some(found) => tracing::debug!(
            "cache hit: key={hash} kind={} bytes={}",
            found.kind,
            found.bytes
        ),
        None => tracing::debug!("cache miss: key={hash}"),
    }
    Ok(entry)
}

pub async fn touch(pool: &SqlitePool, hash: &str) -> Result<bool, sqlx::Error> {
    let now = now_ms();
    let result = sqlx::query!(
        "UPDATE cache_entry SET last_used_at = ? WHERE hash = ?",
        now,
        hash
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn total_bytes(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let row = sqlx::query!(r#"SELECT COALESCE(SUM(bytes), 0) as "total!: i64" FROM cache_entry"#)
        .fetch_one(pool)
        .await?;
    Ok(row.total)
}

pub async fn pinned_bytes(pool: &SqlitePool, releasing_states: &str) -> Result<i64, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT COALESCE(SUM(bytes), 0) as "total!: i64"
           FROM cache_entry
           WHERE hash IN (SELECT r.hash FROM cache_ref r
                          JOIN render_job j ON j.id = r.job_id
                          WHERE instr(?, ',' || j.state || ',') = 0)"#,
        releasing_states
    )
    .fetch_one(pool)
    .await?;
    Ok(row.total)
}

pub async fn lru_evictable(
    pool: &SqlitePool,
    limit: i64,
    releasing_states: &str,
) -> Result<Vec<CacheEntry>, sqlx::Error> {
    sqlx::query_as!(
        CacheEntry,
        r#"SELECT hash as "hash!", kind, bytes, last_used_at
           FROM cache_entry
           WHERE hash NOT IN (SELECT r.hash FROM cache_ref r
                              JOIN render_job j ON j.id = r.job_id
                              WHERE instr(?, ',' || j.state || ',') = 0)
           ORDER BY last_used_at ASC, hash ASC
           LIMIT ?"#,
        releasing_states,
        limit
    )
    .fetch_all(pool)
    .await
}

pub async fn remove(pool: &SqlitePool, hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM cache_entry WHERE hash = ?", hash)
        .execute(pool)
        .await?;
    // Hermes R2 fold-in 6: request-identity mappings die with the wav they
    // point at, or dead rows accumulate forever.
    crate::storage::job::delete_request_cache_for_audio(pool, hash).await?;
    tracing::debug!("cache remove: key={hash}");
    Ok(())
}
