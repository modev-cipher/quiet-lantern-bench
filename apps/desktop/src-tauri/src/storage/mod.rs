pub mod book;
pub mod cache;
pub mod chapter_render;
pub mod export;
pub mod job;
pub mod learning_profile;
pub mod models;
pub mod paths;
pub mod settings;
pub mod voice;
pub mod voice_journal;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
pub use sqlx::SqlitePool;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn open_pool(db_path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    // Migrations run on their own FK-OFF connection (the sqlite.org
    // lang_altertable §7 rebuild procedure): sqlx-sqlite wraps every
    // migration in a transaction — an in-file `PRAGMA foreign_keys` is a
    // no-op there — and with FKs ON a table rebuild's DROP would
    // cascade-delete child rows (migration 0024 rebuilds `voice_session`,
    // whose transcripts must survive it).
    let migrate_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone().foreign_keys(false))
        .await?;
    MIGRATOR.run(&migrate_pool).await?;
    migrate_pool.close().await;
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await?;
    Ok(pool)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F6 migration 0024 rebuild-preservation pin: a database with LIVE
    /// voice_session + voice_transcript rows crosses the rebuild with every
    /// row intact and a clean foreign_key_check — the failure this guards is
    /// the FK-ON cascade wiping voice_transcript when the old table drops
    /// (the reason open_pool migrates on a foreign_keys=OFF connection).
    #[tokio::test]
    async fn the_voice_session_rebuild_preserves_rows_and_fk_integrity() {
        use sqlx::sqlite::SqliteConnectOptions;
        use sqlx::Row;
        let dir = tempfile::tempdir().unwrap();
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join("rebuild.db"))
            .create_if_missing(true)
            .foreign_keys(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        for migration in MIGRATOR.iter() {
            if migration.version >= 24 {
                break;
            }
            sqlx::raw_sql(&migration.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("migration {} applies: {e}", migration.version));
        }
        for seed in [
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at)
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1)",
            "INSERT INTO voice_profile (id, name, engine, kind, created_at)
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1)",
            "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id,
                                     output_format, state, created_at, updated_at)
             VALUES ('j1', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'done', 1, 1)",
            "INSERT INTO voice_session (id, book_id, job_id, provider_base_url, started_at)
             VALUES ('s1', 'b1', 'j1', 'https://p.example', 1)",
            "INSERT INTO voice_transcript (session_id, turn_idx, role, text, created_at)
             VALUES ('s1', 0, 'user', 'hello', 1)",
        ] {
            sqlx::raw_sql(seed).execute(&pool).await.unwrap();
        }

        let rebuild = MIGRATOR
            .iter()
            .find(|m| m.version == 24)
            .expect("migration 0024 is embedded");
        sqlx::raw_sql(&rebuild.sql).execute(&pool).await.unwrap();

        let (sessions, turns): (i64, i64) = (
            sqlx::query_scalar("SELECT COUNT(*) FROM voice_session")
                .fetch_one(&pool)
                .await
                .unwrap(),
            sqlx::query_scalar("SELECT COUNT(*) FROM voice_transcript")
                .fetch_one(&pool)
                .await
                .unwrap(),
        );
        assert_eq!(
            (sessions, turns),
            (1, 1),
            "the rebuild must preserve every session AND transcript row"
        );
        let violations = sqlx::raw_sql("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(
            violations.is_empty(),
            "the rebuilt schema must pass foreign_key_check"
        );
        sqlx::raw_sql(
            "INSERT INTO voice_session (id, book_id, job_id, provider_base_url, started_at)
             VALUES ('s2', NULL, NULL, 'https://p.example', 2)",
        )
        .execute(&pool)
        .await
        .expect("NULL book/job insert is the point of the rebuild");
        let null_book: Option<String> =
            sqlx::query("SELECT book_id FROM voice_session WHERE id = 's2'")
                .fetch_one(&pool)
                .await
                .unwrap()
                .get(0);
        assert_eq!(null_book, None);
    }

    #[test]
    fn migration_versions_strictly_increase() {
        let mut previous: Option<i64> = None;
        for migration in MIGRATOR.iter() {
            if let Some(previous) = previous {
                assert!(
                    migration.version > previous,
                    "migration {} ({}) does not come after {previous}; \
                     two branches picked the same number",
                    migration.version,
                    migration.description
                );
            }
            previous = Some(migration.version);
        }
        assert!(previous.is_some(), "no migrations were embedded");
    }

    #[tokio::test]
    async fn script_chunk_checkpoint_cascades_on_job_delete() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cascade_test.db");
        let pool = open_pool(&db_path).await.unwrap();

        for seed in [
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at)
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1)",
            "INSERT INTO voice_profile (id, name, engine, kind, created_at)
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1)",
            "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id,
                                     output_format, state, created_at, updated_at)
             VALUES ('j1', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'done', 1, 1)",
            "INSERT INTO script_chunk_checkpoint (job_id, chunk_idx, units_fingerprint, total_chunks, outcome_json, summary, created_at)
             VALUES ('j1', 0, 'fp1', 1, '{}', 'sum', 1)",
        ] {
            sqlx::raw_sql(seed).execute(&pool).await.unwrap();
        }

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM script_chunk_checkpoint WHERE job_id = 'j1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);

        sqlx::raw_sql("DELETE FROM render_job WHERE id = 'j1'")
            .execute(&pool)
            .await
            .unwrap();

        let count_after: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM script_chunk_checkpoint WHERE job_id = 'j1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            count_after, 0,
            "script_chunk_checkpoint rows must cascade-delete when render_job is deleted"
        );

        let violations = sqlx::raw_sql("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(violations.is_empty(), "foreign keys must be valid");
    }
}
