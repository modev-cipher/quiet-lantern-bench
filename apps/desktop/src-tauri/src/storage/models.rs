use serde::Serialize;
use sqlx::SqlitePool;

use crate::hardware::learned::LearnedCoefficients;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstall {
    pub model_id: String,
    pub path: String,
    pub sha256: String,
    pub disk_bytes: i64,
    pub source: String,
    pub installed_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HwBenchmark {
    pub profile_json: String,
    pub compute_score: i64,
    pub tok_s_gpu: Option<f64>,
    pub tok_s_cpu: Option<f64>,
    pub rtf_table_json: Option<String>,
    pub fingerprint: Option<String>,
    pub measured_at: i64,
}

pub async fn insert_model_install(
    pool: &SqlitePool,
    install: &ModelInstall,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO model_install (model_id, path, sha256, disk_bytes, source, installed_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(model_id) DO UPDATE SET
           path = excluded.path,
           sha256 = excluded.sha256,
           disk_bytes = excluded.disk_bytes,
           source = excluded.source,
           installed_at = excluded.installed_at",
        install.model_id,
        install.path,
        install.sha256,
        install.disk_bytes,
        install.source,
        install.installed_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_model_install(
    pool: &SqlitePool,
    model_id: &str,
) -> Result<Option<ModelInstall>, sqlx::Error> {
    sqlx::query_as!(
        ModelInstall,
        r#"SELECT model_id as "model_id!", path, sha256, disk_bytes, source, installed_at
           FROM model_install WHERE model_id = ?"#,
        model_id
    )
    .fetch_optional(pool)
    .await
}

pub async fn list_model_installs(pool: &SqlitePool) -> Result<Vec<ModelInstall>, sqlx::Error> {
    sqlx::query_as!(
        ModelInstall,
        r#"SELECT model_id as "model_id!", path, sha256, disk_bytes, source, installed_at
           FROM model_install ORDER BY installed_at"#
    )
    .fetch_all(pool)
    .await
}

pub async fn delete_model_install(pool: &SqlitePool, model_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM model_install WHERE model_id = ?", model_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn upsert_hw_benchmark(
    pool: &SqlitePool,
    benchmark: &HwBenchmark,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO hw_benchmark (id, profile_json, compute_score, tok_s_gpu, tok_s_cpu, rtf_table_json, fingerprint, measured_at)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET profile_json = excluded.profile_json,
                                       compute_score = excluded.compute_score,
                                       tok_s_gpu = excluded.tok_s_gpu,
                                       tok_s_cpu = excluded.tok_s_cpu,
                                       fingerprint = excluded.fingerprint,
                                       measured_at = excluded.measured_at",
        benchmark.profile_json,
        benchmark.compute_score,
        benchmark.tok_s_gpu,
        benchmark.tok_s_cpu,
        benchmark.rtf_table_json,
        benchmark.fingerprint,
        benchmark.measured_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_hw_benchmark(
    pool: &SqlitePool,
    fingerprint: &str,
) -> Result<Option<HwBenchmark>, sqlx::Error> {
    let row = sqlx::query_as!(
        HwBenchmark,
        r#"SELECT profile_json, compute_score, tok_s_gpu, tok_s_cpu, rtf_table_json, fingerprint, measured_at
           FROM hw_benchmark WHERE id = 1"#
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.filter(|row| row.fingerprint.as_deref() == Some(fingerprint)))
}

pub async fn get_rtf_table(pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query!("SELECT rtf_table_json FROM hw_benchmark WHERE id = 1")
        .fetch_optional(pool)
        .await?;
    Ok(row.and_then(|r| r.rtf_table_json))
}

pub async fn load_learned(pool: &SqlitePool) -> Result<LearnedCoefficients, sqlx::Error> {
    Ok(LearnedCoefficients::from_json(
        get_rtf_table(pool).await?.as_deref(),
    ))
}

pub async fn update_learned<F>(pool: &SqlitePool, apply: F) -> Result<(), sqlx::Error>
where
    F: FnOnce(&mut LearnedCoefficients),
{
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let row = sqlx::query!("SELECT rtf_table_json FROM hw_benchmark WHERE id = 1")
        .fetch_optional(&mut *tx)
        .await?;
    let mut coefficients =
        LearnedCoefficients::from_json(row.and_then(|r| r.rtf_table_json).as_deref());
    apply(&mut coefficients);
    let json = coefficients
        .to_json()
        .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let now = crate::storage::now_ms();
    sqlx::query!(
        "INSERT INTO hw_benchmark (id, profile_json, compute_score, rtf_table_json, measured_at)
         VALUES (1, '{}', 0, ?, ?)
         ON CONFLICT(id) DO UPDATE SET rtf_table_json = excluded.rtf_table_json,
                                       measured_at = excluded.measured_at",
        json,
        now,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}
