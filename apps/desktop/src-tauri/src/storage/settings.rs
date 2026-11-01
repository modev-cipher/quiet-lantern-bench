use sqlx::SqlitePool;

use super::now_ms;

pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    let now = now_ms();
    sqlx::query!(
        "INSERT INTO app_setting (key, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        key,
        value,
        now,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query!("SELECT value FROM app_setting WHERE key = ?", key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.value))
}

pub async fn delete_setting(pool: &SqlitePool, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM app_setting WHERE key = ?", key)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_settings_with_value(
    pool: &SqlitePool,
    prefix: &str,
    value: &str,
) -> Result<(), sqlx::Error> {
    let pattern = format!("{prefix}%");
    sqlx::query!(
        "DELETE FROM app_setting WHERE key LIKE ? AND value = ?",
        pattern,
        value,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_settings_with_prefix(
    pool: &SqlitePool,
    prefix: &str,
) -> Result<(), sqlx::Error> {
    let pattern = format!("{prefix}%");
    sqlx::query!("DELETE FROM app_setting WHERE key LIKE ?", pattern)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_settings_with_prefix(
    pool: &SqlitePool,
    prefix: &str,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    let pattern = format!("{prefix}%");
    let rows = sqlx::query!(
        r#"SELECT key as "key!", value FROM app_setting WHERE key LIKE ? ORDER BY key"#,
        pattern
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.key, r.value)).collect())
}
